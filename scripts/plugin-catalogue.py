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
"""
import json
import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent


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


def main() -> None:
    json.dump(build_catalogue(), sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
