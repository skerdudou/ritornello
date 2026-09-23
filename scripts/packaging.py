#!/usr/bin/env python3
"""Copies what deploy/packaging.toml says a component carries.

Split out of package-release.sh because reading TOML with sed is how a
manifest quietly stops describing reality. Everything here is driven by the
file; nothing about a component is written twice.

`entries()` is the one reading of a component's section. `stage()` copies
what it returns, and scripts/install-inventory.py describes what it returns,
so what an archive carries and what the published inventory says it carries
are the same list rather than two readings that could drift apart.
"""
import shutil
import sys
import tomllib
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = tomllib.loads((ROOT / "deploy" / "packaging.toml").read_text())


class Entry(NamedTuple):
    """One file of a staged archive.

    `archive_path` is relative to the staging directory, with no leading
    `./` or `/`. `kind` says why the file is there, which is what the
    inventory needs and a bare (path, source) pair would lose:
    - "tree": lands at `/<archive_path>` on the device;
    - "binary": an `extra_binaries` entry, also landing at `/<archive_path>`;
    - "initial_config": written by the core only when the target is absent;
    - "example": documentation, never installed.
    """

    archive_path: str
    source: Path
    kind: str


def entries(section: dict, bindir: Path) -> list[Entry]:
    out: list[Entry] = []
    # The tree: files that land where they will live on the device, so that
    # installing is one `tar -C /`. A directory (the input presets) expands
    # file by file, here and not at copy time, so that a file added to it is
    # described by the inventory the moment it is carried by the archive.
    for entry in section.get("tree", []):
        src = ROOT / entry["from"]
        if src.is_dir():
            for f in sorted(p for p in src.rglob("*") if p.is_file()):
                rel = f.relative_to(src).as_posix()
                out.append(Entry(f"{entry['to']}/{rel}", f, "tree"))
        else:
            out.append(Entry(entry["to"], src, "tree"))
    # Extra binaries: the files plugin's root mount helper and the core's
    # updater, which live outside the plugins directory on purpose.
    for entry in section.get("extra_binaries", []):
        out.append(Entry(entry["to"], bindir / entry["name"], "binary"))
    # Initial configuration: the same shape as `examples`, in its own
    # directory, so the core can tell "write this if the target is absent"
    # from "this is documentation" without a parser. Outside the tree, like
    # examples: `tar -C /` must never overwrite what the operator wrote.
    initial = section.get("initial_config", [])
    for path in initial:
        out.append(Entry(f"initial-config/{Path(path).name}", ROOT / path, "initial_config"))
    # Examples stay OUT of the tree: they must never overwrite what the
    # operator wrote, so they cannot be inside what `tar -C /` extracts.
    for example in section.get("examples", []):
        # Not duplicated: a file declared as initial configuration is already
        # in the archive, and one copy is enough for both purposes.
        if example in initial:
            continue
        out.append(Entry(f"examples/{Path(example).name}", ROOT / example, "example"))
    return out


def stage(section: dict, out: Path, bindir: Path) -> None:
    for e in entries(section, bindir):
        dst = out / e.archive_path
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(e.source, dst)


def plugin_block(name: str) -> str:
    """The `[[plugin]]` block of `name` in deploy/plugins.example.toml: what
    package-release.sh writes as `plugins.toml.fragment` and what the
    inventory publishes as `block`.

    From the `[[plugin]]` line to the `exec` line inclusive, each line
    newline-terminated; comments above a block belong to no block. This was
    an awk program in package-release.sh; it moved here so that the fragment
    and the inventory are one function, not two copies of a rule. Carriage
    returns are dropped first, for the reason package-release.sh gives: a
    CRLF checkout would otherwise match no `name =` line at all.
    """
    text = (ROOT / "deploy" / "plugins.example.toml").read_bytes().decode("utf-8")
    out, blk, keep = "", "", False
    for line in text.replace("\r", "").split("\n"):
        if line.startswith("[[plugin]]"):
            blk, keep = "", False
        blk += line + "\n"
        if line == f'name = "{name}"':
            keep = True
        if line.startswith("exec") and keep:
            out += blk
            keep = False
    return out


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
    elif cmd == "fragment":
        # Written to stdout as bytes: a text-mode stdout on Windows would
        # turn every "\n" back into the CRLF this function just removed.
        sys.stdout.buffer.write(plugin_block(sys.argv[2]).encode("utf-8"))
    else:
        print(f"unknown command {cmd}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
