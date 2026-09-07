#!/usr/bin/env bash
# Builds the release archives of ONE target from deploy/packaging.toml.
#
# It lives here rather than inside the workflow YAML for the same reason
# scripts/ci-local.sh does: it must be runnable on a development machine and
# its output looked at, instead of being discovered by pushing a tag.
#
# Each archive holds the tree as it will exist on the device, so installing is
# one `tar -C /`. What must never be overwritten — stations.toml,
# input-bindings.toml, plugins.toml — is deliberately NOT in that tree: the
# example files and the plugins.toml fragment sit beside it, to be copied by
# hand.
set -euo pipefail

TARGET="${1:?usage: package-release.sh <cargo-target-triple> <arch-label>}"
ARCH="${2:?usage: package-release.sh <cargo-target-triple> <arch-label>}"

cd "$(dirname "$0")/.."
# `.gitattributes` normalizes *.sh/*.awk/*.service to LF but not *.toml, so a
# checkout with core.autocrlf=true (the common Windows default) hands this
# script CRLF-terminated TOML. `tr -d '\r'` keeps every value extracted below
# free of a trailing carriage return regardless of the checkout that produced
# the working tree.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | tr -d '\r' | head -1)
[ -n "$VERSION" ] || { echo "no version in [workspace.package]" >&2; exit 1; }

BIN="target/$TARGET/release"
OUT="release/$ARCH"
rm -rf "$OUT"
mkdir -p "$OUT"

# The plugin list comes from plugins.example.toml, the same source deploy.sh
# uses. Deriving it is what stops the two from diverging.
mapfile -t PLUGINS < <(sed -n 's/^name = "\(.*\)"/\1/p' deploy/plugins.example.toml | tr -d '\r')
[ "${#PLUGINS[@]}" -gt 0 ] || { echo "no plugin found in plugins.example.toml" >&2; exit 1; }

stage_plugin() {
  local name="$1" dir="$2"
  mkdir -p "$dir/usr/local/lib/ritornello/plugins"
  cp "$BIN/ritornello-plugin-$name" "$dir/usr/local/lib/ritornello/plugins/"
  # Locales, when the plugin has any. Three of them legitimately have none.
  if [ -d "deploy/locales/$name" ]; then
    mkdir -p "$dir/etc/ritornello/locales/$name"
    cp -r "deploy/locales/$name/." "$dir/etc/ritornello/locales/$name/"
  fi
  python3 scripts/packaging.py stage "$name" "$dir" "$BIN"
  # The block to append to /etc/ritornello/plugins.toml. Installing a plugin
  # whose block nobody adds means a plugin that ships and never starts, in
  # silence — a mistake this repository's own documentation records making
  # three times.
  # Filtered through tr first for the same CRLF reason as above: an
  # unstripped source would make `$0 == "name = \"" n "\""` miss every match
  # (the line actually ends in a carriage return) and leave the fragment
  # empty, which the guard below would then also catch.
  tr -d '\r' < deploy/plugins.example.toml | awk -v n="$name" '
    /^\[\[plugin\]\]/ { blk=""; keep=0 }
    { blk = blk $0 "\n" }
    $0 == "name = \"" n "\"" { keep=1 }
    /^exec/ && keep { printf "%s", blk; keep=0 }
  ' > "$dir/plugins.toml.fragment"
  [ -s "$dir/plugins.toml.fragment" ] || { echo "no plugins.toml block for $name" >&2; exit 1; }
}

pack() { # <staging dir> <archive base name>
  local archive="$OUT/$2-$VERSION-$ARCH.tar.gz"
  # `--owner=root --group=root --numeric-owner`, and this is a security
  # property rather than tidiness: tar records the uid/gid of every entry,
  # and GNU tar **restores** them when the extraction runs as the superuser
  # (`--same-owner` is root's default). These archives are built by an
  # unprivileged CI user, so without this the documented `sudo tar -C /`
  # would land /usr/local/bin/ritornello-core, the plugin binaries, the
  # root-run ritornello-media-mount helper, the systemd units and — worst —
  # /etc/polkit-1/rules.d/*.rules owned by the builder's uid. Those rules
  # files are JavaScript that polkitd evaluates as root: a file root executes
  # and a non-root uid can rewrite is a local privilege escalation.
  # `deploy.sh` installs everything `-o root -g root` for exactly this
  # reason; the release path must not be the lax one.
  tar -C "$1" --owner=root --group=root --numeric-owner -czf "$archive" .
  # Asserted and not merely flagged: a guard nobody has seen fail is a guard
  # nobody should trust, and a flag silently dropped by a future edit would
  # leave no trace at all. `--numeric-owner` on the listing too, so the
  # column holds uid/gid instead of whatever names happen to resolve on the
  # machine that reads the archive.
  local foreign
  foreign=$(tar -tvzf "$archive" --numeric-owner | awk '$2 != "0/0"')
  if [ -n "$foreign" ]; then
    echo "$archive holds entries not owned by root — sudo tar -C / would install them under a foreign uid:" >&2
    echo "$foreign" >&2
    exit 1
  fi
  # The property that makes `sudo tar -C /` safe, asserted rather than
  # promised: the tree an archive extracts must never contain a file the
  # operator writes. A staging mistake would otherwise ship an archive that
  # silently reverts someone's stations or their learned key bindings.
  if tar -tzf "$archive" | grep -Eq '^\./etc/ritornello/(stations|input-bindings|plugins)\.toml$'; then
    echo "$archive would overwrite operator data" >&2
    exit 1
  fi
  rm -rf "$1"
}

# --- the core -------------------------------------------------------------
CORE=$(mktemp -d)
mkdir -p "$CORE/usr/local/bin"
cp "$BIN/ritornello-core" "$CORE/usr/local/bin/"
# Everything else the core carries — its unit, its polkit rule, its own and
# the shared locale packs — is named by the manifest, not repeated here.
python3 scripts/packaging.py stage-core "$CORE" "$BIN"
pack "$CORE" "ritornello-core"

# --- one archive per plugin ----------------------------------------------
for p in "${PLUGINS[@]}"; do
  D=$(mktemp -d)
  stage_plugin "$p" "$D"
  pack "$D" "ritornello-plugin-$p"
done

# --- the bundle of all plugins -------------------------------------------
ALL=$(mktemp -d)
for p in "${PLUGINS[@]}"; do stage_plugin "$p" "$ALL"; done
rm -f "$ALL/plugins.toml.fragment"
mkdir -p "$ALL/examples"
cp deploy/plugins.example.toml "$ALL/examples/"
pack "$ALL" "ritornello-plugins"

ls -l "$OUT"
echo "OK — $(ls "$OUT" | wc -l) archives for $ARCH"
