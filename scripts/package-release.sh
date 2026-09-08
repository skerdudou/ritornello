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
# The PRODUCT number: the name of the delivery, used only for the bundled
# archive of all plugins. Each shipped component's own archive is named after
# that component's own version instead — see crate_version() below.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | tr -d '\r' | head -1)
[ -n "$VERSION" ] || { echo "no version in [workspace.package]" >&2; exit 1; }

# The version a shipped component declares for itself. Not the product number:
# see the comment on [workspace.package] version. A component that inherits
# would land here as the literal `version.workspace = true`, which no `sed`
# below matches, so the guard fires rather than naming an archive `-true-`.
crate_version() { # <crate directory name>
  local v
  v=$(sed -n 's/^version = "\(.*\)"/\1/p' "crates/$1/Cargo.toml" | tr -d '\r' | head -1)
  [ -n "$v" ] || { echo "crates/$1 declares no version of its own" >&2; exit 1; }
  # Major and minor must stay on the product generation. Asserted here as well
  # as in version_coherence.rs, because this script runs without cargo and a
  # release must not be buildable with a component off its generation.
  [ "${v%.*}" = "${VERSION%.*}" ] \
    || { echo "crates/$1 is $v, off the product generation ${VERSION%.*}" >&2; exit 1; }
  echo "$v"
}

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

pack() { # <staging dir> <archive base name> <version>
  local archive="$OUT/$2-$3-$ARCH.tar.gz"
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
  #
  # `--mode` for the same reason as `--owner`/`--group` beside it: the
  # builder's metadata must not reach the device. This worktree sits on a
  # 9p/drvfs mount that reports every source file as 777, and
  # packaging.py's copytree carries that through — so archives built here
  # shipped world-writable locale directories, and would have shipped a
  # world-writable polkit rule: JavaScript polkitd runs as root that any
  # local user could rewrite. `mktemp -d` supplies the opposite failure,
  # 0700 on the archive's own `./` entry, which GNU tar then applies to `/`
  # itself.
  #
  # `u+rwX,go=rX` is chmod's symbolic form, `X` meaning "execute only where
  # it already applies": a directory or a file that already carries an
  # execute bit anywhere lands at 755, and a file with no execute bit at
  # all lands at 644. Measured on this machine rather than assumed — 0700
  # and 0777 both become 755, and because every source file here already
  # carries an execute bit (the same 9p/drvfs quirk noted above), every
  # entry from this machine lands at 755, unit files and locale catalogs
  # included; a checkout where git's own mode bit says 644 would keep
  # those at 644 instead. Either way, nothing group- or world-writable
  # reaches the archive.
  tar -C "$1" --owner=root --group=root --numeric-owner --mode='u+rwX,go=rX' -czf "$archive" .
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
  # Every directory 0755, and nothing group- or world-writable anywhere.
  # Asserted rather than promised, for the reason the guard above gives:
  # a future staging step, or a different developer's filesystem, would
  # otherwise change what lands on the device with nothing to notice.
  local badmode
  badmode=$(tar -tvzf "$archive" --numeric-owner | awk '
    ($1 ~ /^d/ && $1 != "drwxr-xr-x") ||
    substr($1, 6, 1) == "w" ||
    substr($1, 9, 1) == "w"')
  if [ -n "$badmode" ]; then
    echo "$archive holds entries whose mode is not safe to extract as root — a directory that is not 0755, or something group- or world-writable:" >&2
    echo "$badmode" >&2
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
pack "$CORE" "ritornello-core" "$(crate_version ritornello-core)"

# --- one archive per plugin ----------------------------------------------
for p in "${PLUGINS[@]}"; do
  D=$(mktemp -d)
  stage_plugin "$p" "$D"
  pack "$D" "ritornello-plugin-$p" "$(crate_version "ritornello-plugin-$p")"
done

# --- the bundle of all plugins -------------------------------------------
ALL=$(mktemp -d)
for p in "${PLUGINS[@]}"; do stage_plugin "$p" "$ALL"; done
rm -f "$ALL/plugins.toml.fragment"
mkdir -p "$ALL/examples"
cp deploy/plugins.example.toml "$ALL/examples/"
# The PRODUCT number, not any single plugin's: this bundle is nobody's own
# component, it exists for a first installation, and the updater ignores it.
pack "$ALL" "ritornello-plugins" "$VERSION"

ls -l "$OUT"
echo "OK — $(ls "$OUT" | wc -l) archives for $ARCH"
