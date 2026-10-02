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
      "companions": [<component> + {"with": <plugin name>}, ...],
      "packs": [{"language", "version", "archive"}, ...]
    }

    component = {
      "name", "version",
      "archive": "<base>-<version>-{arch}.tar.gz"   ({arch} is literal: the
                  installer substitutes the device's architecture label),
      "files": [{"archive_path", "dest", "mode", "owner", "privileged"}],
      "initial_config": [{"archive_path", "target"}],
      "enable": [<unit names>], "mount_root": <path or null>,
      "block": <the plugins.toml [[plugin]] block, or null for the core
                and for a companion>
    }

A companion (`[companions.<name>]` in deploy/packaging.toml) ships beside
the plugin its `with` names, in its own archive `ritornello-<name>-...`, and
carries no binary beyond its `extra_binaries`: no plugin binary, no
plugins.toml block, no initial configuration. It holds what is privileged
about its plugin, so a plugin's own entry never carries a privileged file;
`--self-test` refuses one that does.

`archive_path` is written WITHOUT a leading `./`, although the archives are
built with `tar -C <dir> .` and so name their members `./usr/...`: the
installer's tar reader strips a leading `./` before looking a member up, and
the bare form is the one `dest` is derived from (`dest = "/" + archive_path`).

`files` holds what lands on the device: the component's own binary (none for
a companion), its tree and its extra binaries. Examples are never installed and are absent; so is
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


def privileged_tree_dest(path: str) -> bool:
    """Exactly the rule of packaging_manifest.rs's `privileged_dest`: a
    unit, a polkit rule, or a root-run location outside the plugins
    directory."""
    return path.startswith((UNIT_DIR, POLKIT_DIR, "usr/local/bin/")) or (
        path.startswith("usr/local/lib/ritornello/") and not path.startswith(PLUGINS_DIR + "/")
    )


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


def component(name: str, section: dict, version: str, main_binary, block, base: str) -> dict:
    # `main_binary` is None for a companion: package-release.sh adds no binary
    # of its own to that archive, everything it carries is in its section.
    files = [file_entry(main_binary, binary=True, privileged=False)] if main_binary else []
    initial = []
    for e in packaging.entries(section, Path("bin")):
        if e.kind == "tree":
            # Exactly the rule of packaging_manifest.rs's
            # `every_privileged_plugin_agrees_with_packaging_toml`.
            files.append(file_entry(e.archive_path, False, privileged_tree_dest(e.archive_path)))
        elif e.kind == "binary":
            files.append(file_entry(e.archive_path, True, True))
        elif e.kind == "initial_config":
            # Not `base`: that name is the archive's, and reusing it here once
            # named generic-input's archive after its example file.
            config = e.archive_path.removeprefix("initial-config/")
            initial.append({"archive_path": e.archive_path, "target": initial_config_target(config)})
        # "example": documentation, never installed.
    enable = list(section.get("enable", []))
    units = {f["archive_path"].removeprefix(UNIT_DIR) for f in files if f["archive_path"].startswith(UNIT_DIR)}
    for unit in enable:
        if unit not in units:
            raise SystemExit(f"{name}: enables {unit}, which it does not place under /{UNIT_DIR}")
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
        "ritornello-core",
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
                f"ritornello-plugin-{name}",
            )
        )
    companions = []
    for name, section in packaging.companions().items():
        with_ = section.get("with")
        if with_ not in order:
            raise SystemExit(f"companion {name}: with = {with_!r} names no plugin of plugins.example.toml")
        # The archive and the crate share one name, as package-release.sh
        # builds it: `ritornello-<name>`, versioned by that crate.
        crate = f"ritornello-{name}"
        entry = component(
            name,
            section,
            first_version(ROOT / "crates" / crate / "Cargo.toml"),
            None,
            None,
            crate,
        )
        entry["with"] = with_
        companions.append(entry)
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
        "companions": companions,
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

    A companion is staged by running `packaging.py stage-companion`, the very
    command package-release.sh builds its archive with, so a command that
    staged the wrong section, or nothing, is caught here too.
    """
    inv = build()
    manifest = packaging.MANIFEST
    problems = []

    def stage_section(section):
        return lambda out, bindir: packaging.stage(section, out, bindir)

    def stage_companion(name):
        def run(out, bindir):
            subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "packaging.py"), "stage-companion", name, str(out), str(bindir)],
                check=True,
            )

        return run

    # (inventory entry, its section, how package-release.sh stages it,
    # whether package-release.sh adds a main binary of its own)
    components = [(inv["core"], manifest["core"], stage_section(manifest["core"]), True)]
    for p in inv["plugins"]:
        section = manifest["plugins"].get(p["name"], {})
        components.append((p, section, stage_section(section), True))
    for c in inv["companions"]:
        components.append((c, packaging.companions()[c["name"]], stage_companion(c["name"]), False))
    if not inv["companions"]:
        problems.append("no companion in the inventory: the files plugin's mount helper is described by nothing")
    for comp, section, stage_it, has_main in components:
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bindir, out = tmp / "bin", tmp / "out"
            bindir.mkdir()
            out.mkdir()
            for e in section.get("extra_binaries", []):
                (bindir / e["name"]).write_text("fake\n")
            stage_it(out, bindir)
            staged = {p.relative_to(out).as_posix() for p in out.rglob("*") if p.is_file()}
        staged = {p for p in staged if not p.startswith("examples/")}
        own = comp["files"][1:] if has_main else comp["files"]
        described = {f["archive_path"] for f in own} | {c["archive_path"] for c in comp["initial_config"]}
        for p in sorted(staged - described):
            problems.append(f"{comp['name']}: the archive carries {p}, the inventory does not describe it")
        for p in sorted(described - staged):
            problems.append(f"{comp['name']}: the inventory describes {p}, the archive does not carry it")
        if has_main:
            main = comp["files"][0]["archive_path"]
            if main in staged:
                problems.append(f"{comp['name']}: {main} is staged twice, by packaging.py and package-release.sh")
        else:
            # What makes a companion one: no plugins.toml block, no initial
            # configuration, a plugin it ships with, and something privileged
            # to carry for it.
            if comp["block"] is not None or comp["initial_config"]:
                problems.append(f"{comp['name']}: a companion carries no plugins.toml block and no initial configuration")
            if comp.get("with") not in inv["reference_order"]:
                problems.append(f"{comp['name']}: with = {comp.get('with')!r} names no plugin")
            if not any(f["privileged"] for f in comp["files"]):
                problems.append(f"{comp['name']}: a companion that places nothing privileged has no reason to exist")
    # A plugin's own entry never carries a privileged file any more: that is
    # its companion's job, and it is what lets the web UI update the plugin's
    # archive like any other. The same rule as packaging_manifest.rs's
    # `every_privileged_plugin_agrees_with_packaging_toml`.
    for p in inv["plugins"]:
        for f in p["files"]:
            if f["privileged"]:
                problems.append(f"{p['name']}: the plugin's own entry places the privileged {f['dest']}; that belongs to a companion")
    # Each archive under the name package-release.sh gives it
    # (`pack "$D" <base> "$(crate_version <base>)"`), restated here rather
    # than read back from build(): the inventory once named generic-input's
    # archive after its example file, and ritornello-install only found out
    # on the device, fetching a file no release could ever carry.
    expected = [(inv["core"], "ritornello-core")]
    expected += [(p, f"ritornello-plugin-{p['name']}") for p in inv["plugins"]]
    expected += [(c, f"ritornello-{c['name']}") for c in inv["companions"]]
    for comp, base in expected:
        want = f"{base}-{first_version(ROOT / 'crates' / base / 'Cargo.toml')}-{{arch}}.tar.gz"
        if comp["archive"] != want:
            problems.append(f"{comp['name']}: the inventory names its archive {comp['archive']}, package-release.sh builds {want}")
    for comp, section, stage_it, has_main in components:
        if has_main and comp["name"] != "core":
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
