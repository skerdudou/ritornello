#!/usr/bin/env python3
"""Builds inventory.json: what each component of a release installs.

Printed to stdout as JSON; the publish job of .github/workflows/ci.yml
redirects it to assets/inventory.json, beside catalogue.json and for the same
reasons (built once, architecture-independent, after the filtering step that
deletes whatever it does not recognise). `ritornello-install` reads it to
know what to place on the device and what to remove.

It is generated from deploy/packaging.toml by the very `entries()` that
scripts/packaging.py stages the archives from, and `block` is the very
`plugin_block()` that package-release.sh writes as plugins.toml.fragment, so
an archive and its description cannot disagree. `--self-test` stages every
component and proves it; the Rust suite runs that (packaging_manifest.rs).

Format 1:

    {
      "format": 1,
      "product": "<[workspace.package] version>",
      "reference_order": [<plugin names, in plugins.example.toml's order>],
      "core": <component>,
      "plugins": [<component>, ... in reference order],
      "packs": [{"language", "version", "archive"}, ...]
    }

    component = {
      "name", "version",
      "archive": "<base>-<version>-{arch}.tar.gz"   ({arch} is literal: the
                  installer substitutes the device's architecture label),
      "files": [{"archive_path", "dest", "mode", "owner", "privileged"}],
      "initial_config": [{"archive_path", "target"}],
      "enable": [<unit names>], "mount_root": <path or null>,
      "block": <the plugins.toml [[plugin]] block, or null for the core>
    }

`archive_path` is written WITHOUT a leading `./`, although the archives are
built with `tar -C <dir> .` and so name their members `./usr/...`: the
installer's tar reader strips a leading `./` before looking a member up, and
the bare form is the one `dest` is derived from (`dest = "/" + archive_path`).

`files` holds what lands on the device: the component's own binary, its tree
and its extra binaries. Examples are never installed and are absent; so is
plugins.toml.fragment, whose content is `block`. Initial configuration is
listed apart, because it is written only when the target is absent, under
the operating name `target` in the plugin's data directory.
"""
import json
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

# No scripts/__pycache__/ left behind in the working tree by the import below.
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import packaging  # noqa: E402  (scripts/packaging.py, not the PyPI package)

ROOT = packaging.ROOT

# Where each shipped binary lands. package-release.sh copies the component's
# own binary itself (it is not in packaging.toml, since every component has
# exactly one); these are the directories it copies into.
CORE_BINARY = "usr/local/bin/ritornello-core"
PLUGINS_DIR = "usr/local/lib/ritornello/plugins"

UNIT_DIR = "etc/systemd/system/"
POLKIT_DIR = "etc/polkit-1/rules.d/"


def first_version(path: Path) -> str:
    """The first `version = "..."` line of a file, as package-release.sh's
    `sed -n 's/^version = "\\(.*\\)"/\\1/p' | head -1` reads it."""
    for line in path.read_bytes().decode("utf-8").replace("\r", "").split("\n"):
        m = re.match(r'^version = "(.*)"(.*)$', line)
        if m:
            return m.group(1) + m.group(2)
    raise SystemExit(f"{path} declares no version of its own")


def reference_order() -> list[str]:
    """The `name = "..."` lines of plugins.example.toml, in order: the same
    `sed` package-release.sh and deploy.sh derive the plugin list from."""
    text = (ROOT / "deploy" / "plugins.example.toml").read_bytes().decode("utf-8")
    names = []
    for line in text.replace("\r", "").split("\n"):
        m = re.match(r'^name = "(.*)"(.*)$', line)
        if m:
            names.append(m.group(1) + m.group(2))
    if not names:
        raise SystemExit("no plugin found in plugins.example.toml")
    return names


def pack_version(lang: str, packs: dict) -> str:
    # The `[<lang>]` section's `version`, as pack_version() in
    # package-release.sh reads it.
    v = packs.get(lang, {}).get("version")
    if not v:
        raise SystemExit(f"deploy/language-packs.toml declares no version for [{lang}]")
    return v


def owner(path: str) -> str:
    # The unprivileged core rewrites what is under etc/ritornello/ (the input
    # presets, on update), so it must own it. Everything else root owns.
    # A location outside these is refused rather than guessed: adding one is
    # a decision to make here, deliberately.
    if path.startswith("etc/ritornello/"):
        return "ritornello:ritornello"
    if path.startswith(("usr/", UNIT_DIR, POLKIT_DIR)):
        return "root:root"
    raise SystemExit(f"{path}: no ownership rule for this location")


def file_entry(path: str, binary: bool, privileged: bool) -> dict:
    return {
        "archive_path": path,
        "dest": "/" + path,
        "mode": "0755" if binary else "0644",
        "owner": owner(path),
        "privileged": privileged,
    }


def initial_config_target(name: str) -> str:
    # The same rule as `initial_config_target` in the core's update module:
    # `stations.example.toml` becomes `stations.toml`, anything else keeps
    # its name.
    return name[: -len(".example.toml")] + ".toml" if name.endswith(".example.toml") else name


def component(name: str, section: dict, version: str, main_binary: str, block) -> dict:
    files = [file_entry(main_binary, binary=True, privileged=False)]
    initial = []
    for e in packaging.entries(section, Path("bin")):
        if e.kind == "tree":
            # Exactly the rule of packaging_manifest.rs's
            # `every_privileged_plugin_agrees_with_packaging_toml`.
            files.append(file_entry(e.archive_path, False, e.archive_path.startswith((UNIT_DIR, POLKIT_DIR))))
        elif e.kind == "binary":
            files.append(file_entry(e.archive_path, True, True))
        elif e.kind == "initial_config":
            base = e.archive_path.removeprefix("initial-config/")
            initial.append({"archive_path": e.archive_path, "target": initial_config_target(base)})
        # "example": documentation, never installed.
    enable = list(section.get("enable", []))
    units = {f["archive_path"].removeprefix(UNIT_DIR) for f in files if f["archive_path"].startswith(UNIT_DIR)}
    for unit in enable:
        if unit not in units:
            raise SystemExit(f"{name}: enables {unit}, which it does not place under /{UNIT_DIR}")
    base = "ritornello-core" if name == "core" else f"ritornello-plugin-{name}"
    return {
        "name": name,
        "version": version,
        "archive": f"{base}-{version}-{{arch}}.tar.gz",
        "files": files,
        "initial_config": initial,
        "enable": enable,
        "mount_root": section.get("mount_root"),
        "block": block,
    }


def build() -> dict:
    manifest = packaging.MANIFEST
    order = reference_order()
    core = component(
        "core",
        manifest["core"],
        first_version(ROOT / "crates" / "ritornello-core" / "Cargo.toml"),
        CORE_BINARY,
        None,
    )
    plugins = []
    for name in order:
        block = packaging.plugin_block(name)
        if not block:
            raise SystemExit(f"no plugins.toml block for {name}")
        plugins.append(
            component(
                name,
                manifest["plugins"].get(name, {}),
                first_version(ROOT / "crates" / f"ritornello-plugin-{name}" / "Cargo.toml"),
                f"{PLUGINS_DIR}/ritornello-plugin-{name}",
                block,
            )
        )
    packs_toml = tomllib.loads((ROOT / "deploy" / "language-packs.toml").read_text(encoding="utf-8"))
    packs = []
    for lang in packs_toml:
        v = pack_version(lang, packs_toml)
        packs.append({"language": lang, "version": v, "archive": f"ritornello-lang-{lang}-{v}.tar.gz"})
    return {
        "format": 1,
        "product": first_version(ROOT / "Cargo.toml"),
        "reference_order": order,
        "core": core,
        "plugins": plugins,
        "packs": packs,
    }


def self_test() -> int:
    """Stages every component with packaging.stage, exactly as
    package-release.sh does, and compares the staged files with the
    inventory's archive paths.

    Left out of the comparison, deliberately and only these:
    - `examples/*`: carried by the archive, never installed, so absent from
      the inventory by design;
    - the component's own binary and `plugins.toml.fragment`: package-release.sh
      adds those itself, outside packaging.stage. The fragment is checked
      instead against `block` below, by running the command
      package-release.sh writes it with.
    """
    inv = build()
    manifest = packaging.MANIFEST
    problems = []
    components = [(inv["core"], manifest["core"])] + [
        (p, manifest["plugins"].get(p["name"], {})) for p in inv["plugins"]
    ]
    for comp, section in components:
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bindir, out = tmp / "bin", tmp / "out"
            bindir.mkdir()
            out.mkdir()
            for e in section.get("extra_binaries", []):
                (bindir / e["name"]).write_text("fake\n")
            packaging.stage(section, out, bindir)
            staged = {p.relative_to(out).as_posix() for p in out.rglob("*") if p.is_file()}
        staged = {p for p in staged if not p.startswith("examples/")}
        main = comp["files"][0]["archive_path"]
        described = {f["archive_path"] for f in comp["files"][1:]} | {
            c["archive_path"] for c in comp["initial_config"]
        }
        for p in sorted(staged - described):
            problems.append(f"{comp['name']}: the archive carries {p}, the inventory does not describe it")
        for p in sorted(described - staged):
            problems.append(f"{comp['name']}: the inventory describes {p}, the archive does not carry it")
        if main in staged:
            problems.append(f"{comp['name']}: {main} is staged twice, by packaging.py and package-release.sh")
        if comp["name"] != "core":
            # The exact command package-release.sh redirects into
            # plugins.toml.fragment, run as it runs it.
            fragment = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "packaging.py"), "fragment", comp["name"]],
                check=True,
                capture_output=True,
            ).stdout.decode("utf-8")
            if comp["block"] != fragment:
                problems.append(f"{comp['name']}: block differs from plugins.toml.fragment")
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    print("self-test: inventory ok")
    return 0


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    if sys.argv[1:]:
        print("usage: install-inventory.py [--self-test]", file=sys.stderr)
        return 2
    # Bytes, not text: a text-mode stdout on Windows would write CRLF.
    sys.stdout.buffer.write((json.dumps(build(), ensure_ascii=False, indent=2) + "\n").encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
