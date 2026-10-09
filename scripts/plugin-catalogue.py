#!/usr/bin/env python3
"""Builds the release-wide catalogue: what each of the ten components is.

Printed to stdout as JSON; the publish job of .github/workflows/ci.yml
redirects it to assets/catalogue.json, built once for the whole release
because the asset is architecture-independent (see that workflow's comment
for the two measured reasons it must be built there and not by
package-release.sh).

The plugin list is read from deploy/plugins.example.toml -- the same source
package-release.sh and deploy.sh already use -- rather than a glob of crate
directories, so a plugin declared there without a matching crate (or the
reverse) fails loudly instead of silently describing the wrong set.

The catalogue describes every declared component, not only the ones a given
release ships: the device resolves each component to the newest release that
carries its own archive, so a component untouched for several deliveries
would otherwise be described by nothing.

The `contracts` entry is the opposite: it lists only the components whose
archive is in *this* release (`--shipped <changed.txt>`), with the wire
versions of the tree this release was cut from. An archive carried by an
older release must never be described with the current tree's numbers, so
a component the release does not ship has no entry, and the device reads
its contracts from the release that carries its archive. Without
`--shipped` (development use) `contracts` is empty. Companions and language
packs speak no wire and never appear.
"""
import json
import pathlib
import re
import sys
import tomllib

# No scripts/__pycache__/ left behind by the import below, and found whatever
# the interpreter's flags (`-I` drops the script directory from the path).
sys.dont_write_bytecode = True
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import packaging  # noqa: E402  (scripts/packaging.py, not the PyPI package)

ROOT = pathlib.Path(__file__).resolve().parent.parent
# Where the two wire-constant files live; `--proto-src <dir>` moves it, for
# tests that mangle a constant on a copy.
PROTO_SRC = ROOT / "crates" / "ritornello-proto" / "src"


def plugin_names() -> list[str]:
    """The plugin list, in the order deploy/plugins.example.toml declares it."""
    manifest = ROOT / "deploy" / "plugins.example.toml"
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    names = [p["name"] for p in data.get("plugin", [])]
    if not names:
        raise SystemExit(f"read no plugin at all from {manifest} -- the parsing is wrong, not the file")
    return names


def crate_metadata(name: str) -> dict:
    """The `[package.metadata.ritornello]` table of the plugin's own crate:
    its kinds, and the English description that is the catalogue's fallback."""
    path = ROOT / "crates" / f"ritornello-plugin-{name}" / "Cargo.toml"
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    return data["package"]["metadata"]["ritornello"]


def french_description(name: str) -> str | None:
    """The plugin's own `plugin_description`, from the French pack shipped
    beside its other strings -- `deploy/locales/<name>/fr.toml`.

    `None` for a plugin with no pack at all (four of them: console,
    nrj-metas, ouifm-metas, radiofrance-metas -- a normal state, not an
    error, asserted by
    `shipped_language_packs_of_an_absent_module_directory_is_empty` in
    `crates/ritornello-i18n/src/layer.rs`), and equally `None` for a pack
    that exists but does not carry that key yet: either way the caller
    falls back to the English text from Cargo.toml.
    """
    pack = ROOT / "deploy" / "locales" / name / "fr.toml"
    if not pack.exists():
        return None
    data = tomllib.loads(pack.read_text(encoding="utf-8"))
    return data.get("plugin_description")


def build_catalogue() -> dict:
    """One entry per declared plugin: its kinds, and its description in one
    language only.

    A single language is deliberate: the catalogue is a release asset, not a
    language pack, and one entry per language would introduce a second
    translation mechanism for a decision this comment records rather than
    hides. French is the language chosen at build time; the day a third
    language exists, this is the decision to reopen.
    """
    components = {}
    for name in plugin_names():
        meta = crate_metadata(name)
        description = french_description(name) or meta["description"]
        components[name] = {"kinds": meta["kinds"], "description": description}
    return {"components": components}


def read_constant(path: pathlib.Path, pattern: str, what: str) -> re.Match:
    """The first line of `path` matching `pattern`; exits if there is none
    (the constants keep a one-line shape on purpose, as changed-components.sh
    relies on)."""
    text = path.read_text(encoding="utf-8").replace("\r", "")
    m = re.search(pattern, text, re.MULTILINE)
    if not m:
        raise SystemExit(f"cannot read {what} from {path}")
    return m


def protocol_version() -> int:
    m = read_constant(
        PROTO_SRC / "lib.rs",
        r"^pub const PROTOCOL_VERSION: u32 = (\d+);",
        "PROTOCOL_VERSION",
    )
    return int(m.group(1))


def contract_versions() -> dict[str, dict]:
    """The five wire contracts of this tree, by lowercase name."""
    path = PROTO_SRC / "contract.rs"
    out = {}
    for name in ("source", "display", "input", "metadata", "admin"):
        const = f"{name.upper()}_CONTRACT"
        m = read_constant(
            path,
            rf"^pub const {const}: ContractVersion = ContractVersion::new\((\d+), *(\d+)\);",
            const,
        )
        out[name] = {"major": int(m.group(1)), "minor": int(m.group(2))}
    return out


def plugin_contracts(name: str, all_contracts: dict) -> list[str]:
    """The contracts a plugin speaks: one per declared kind, plus `admin`
    iff it declares `admin = true`."""
    meta = crate_metadata(name)
    spoken = set(meta["kinds"])
    if meta.get("admin", False):
        spoken.add("admin")
    unknown = spoken - set(all_contracts)
    if unknown:
        raise SystemExit(f"plugin {name} declares kinds with no contract: {sorted(unknown)}")
    return sorted(spoken)


def build_contracts(shipped: list[str]) -> dict:
    """The contracts of the shipped components, named as a device names
    them: `core`, and the plugin's bare name."""
    plugins = plugin_names()
    companions = {f"ritornello-{c}" for c in packaging.companions()}
    contracts = {}
    proto = protocol_version()
    versions = contract_versions()
    for archive in shipped:
        if archive == "ritornello-core":
            contracts["core"] = {"protocol": proto, "contracts": dict(versions)}
        elif archive.startswith("ritornello-plugin-") and archive.removeprefix("ritornello-plugin-") in plugins:
            name = archive.removeprefix("ritornello-plugin-")
            contracts[name] = {
                "protocol": proto,
                "contracts": {c: versions[c] for c in plugin_contracts(name, versions)},
            }
        elif archive in companions or archive.startswith("ritornello-lang-"):
            continue  # no wire
        else:
            raise SystemExit(f"shipped component {archive!r} is neither the core, a declared plugin, a companion nor a language pack")
    return contracts


def main() -> None:
    args = sys.argv[1:]
    shipped: list[str] = []
    global PROTO_SRC
    usage = "usage: plugin-catalogue.py [--shipped <changed.txt>] [--proto-src <dir>]"
    while args:
        if len(args) < 2 or args[0] not in ("--shipped", "--proto-src"):
            raise SystemExit(usage)
        if args[0] == "--shipped":
            lines = pathlib.Path(args[1]).read_text(encoding="utf-8").splitlines()
            shipped = [l.strip() for l in lines if l.strip()]
        else:
            PROTO_SRC = pathlib.Path(args[1])
        args = args[2:]
    catalogue = build_catalogue()
    catalogue["contracts"] = build_contracts(shipped)
    json.dump(catalogue, sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
