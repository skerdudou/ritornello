#!/usr/bin/env python3
"""Copies what deploy/packaging.toml says a component carries.

Split out of package-release.sh because reading TOML with sed is how a
manifest quietly stops describing reality. Everything here is driven by the
file; nothing about a component is written twice.
"""
import shutil
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = tomllib.loads((ROOT / "deploy" / "packaging.toml").read_text())


def copy(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    if src.is_dir():
        shutil.copytree(src, dst, dirs_exist_ok=True)
    else:
        shutil.copy2(src, dst)


def stage(section: dict, out: Path, bindir: Path) -> None:
    # The tree: files that land where they will live on the device, so that
    # installing is one `tar -C /`.
    for entry in section.get("tree", []):
        copy(ROOT / entry["from"], out / entry["to"])
    # Extra binaries: today only the files plugin's root mount helper, which
    # lives outside the plugins directory on purpose.
    for entry in section.get("extra_binaries", []):
        copy(bindir / entry["name"], out / entry["to"])
    # Examples stay OUT of the tree: they must never overwrite what the
    # operator wrote, so they cannot be inside what `tar -C /` extracts.
    for example in section.get("examples", []):
        copy(ROOT / example, out / "examples" / Path(example).name)


def main() -> int:
    cmd = sys.argv[1]
    if cmd == "stage-core":
        # The bindir is no longer optional: the core carries an extra binary
        # since the updater exists. Passing Path() here used to be harmless
        # only because `extra_binaries` was empty for this component.
        stage(MANIFEST["core"], Path(sys.argv[2]), Path(sys.argv[3]))
    elif cmd == "stage":
        name, out, bindir = sys.argv[2], Path(sys.argv[3]), Path(sys.argv[4])
        stage(MANIFEST["plugins"].get(name, {}), out, bindir)
    else:
        print(f"unknown command {cmd}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
