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

cd "$(dirname "$0")/.."

USAGE="usage: package-release.sh <cargo-target-triple> <arch-label> | --languages | --self-test"
SELF_TEST=
LANGUAGES=
case "${1:-}" in
  --self-test)
    # Runs the version guards below against a table of cases and exits,
    # building nothing. Called by the Rust suite (version_coherence.rs),
    # because this script's only other exercise is the release job — which
    # fires on a tag, so without it the guards are first read on the day a
    # release is being cut.
    SELF_TEST=1
    ;;
  --languages)
    # Builds one archive per language pack. Unlike the per-target path below,
    # this one needs neither `$BIN` nor `$ARCH`: a pack is text, with no
    # architecture of its own, and the CI job that calls this path runs
    # outside the per-arch matrix precisely because of that (see ci.yml).
    LANGUAGES=1
    ;;
  *)
    TARGET="${1:?$USAGE}"
    ARCH="${2:?$USAGE}"
    ;;
esac
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
# The generation of a version: major and minor, with any prerelease suffix
# removed FIRST.
#
# Removing it first is the whole point. `${v%.*}` alone cuts at the last dot,
# which answers `0.2` for `0.2.0` but `0.2.1-beta` for `0.2.1-beta.1` — not a
# generation, and equal to no other component's. A beta shipping only the
# component it fixes was therefore refused outright, while the very same
# suffix written without a dot (`0.2.1-beta1`) sailed through: the guard was
# deciding on where the dots fell.
generation() { # <version>
  local core=${1%%-*}
  echo "${core%.*}"
}

# Whether a component may ship under $VERSION. Split out of crate_version so
# --self-test exercises these expressions rather than a copy of them: a
# self-test that restates the rule proves only that it can restate it.
version_fits() { # <label> <component version>   (reads $VERSION)
  local label="$1" v="$2"
  # Major and minor must stay on the product generation. Asserted here as
  # well as in version_coherence.rs, because this script runs without cargo
  # and a release must not be buildable with a component off its generation.
  if [ "$(generation "$v")" != "$(generation "$VERSION")" ]; then
    echo "$label is $v, off the product generation $(generation "$VERSION")" >&2
    return 1
  fi
  # A component need NOT carry the product's prerelease suffix: a beta may
  # ship one component and leave the others where the last finished release
  # left them — the device is offered only what differs, so the others are
  # simply not part of that beta.
  #
  # What it must never do is declare the number the FINISHED release will
  # carry. The device compares versions for equality: a tester installing
  # `0.2.1` out of `v0.2.1-beta.1` would never be given the real `0.2.1`,
  # and would keep the beta's bytes for ever, silently.
  if [ "$VERSION" != "${VERSION%%-*}" ] && [ "$v" = "${VERSION%%-*}" ]; then
    echo "$label is $v inside prerelease $VERSION: the finished ${VERSION%%-*} will carry that same number, so a device installing it here would never replace it" >&2
    return 1
  fi
  return 0
}

# The version a shipped component declares for itself.
crate_version() { # <crate directory name>
  local v
  v=$(sed -n 's/^version = "\(.*\)"/\1/p' "crates/$1/Cargo.toml" | tr -d '\r' | head -1)
  [ -n "$v" ] || { echo "crates/$1 declares no version of its own" >&2; exit 1; }
  version_fits "crates/$1" "$v" || exit 1
  echo "$v"
}

# The version a language pack declares for itself, read from its own section
# of deploy/language-packs.toml rather than a Cargo.toml -- a pack is text,
# not a crate. Scoped to the `[<language>]` section by hand, because that
# file holds one section per language and a plain `sed` over the whole file
# would answer with whichever language's `version =` line came first.
pack_version() { # <language>
  local lang="$1" v
  v=$(awk -v section="[$lang]" '
    $0 == section { found=1; next }
    found && /^\[/ { found=0 }
    found && /^version = / { sub(/^version = "/, ""); sub(/"$/, ""); print; exit }
  ' deploy/language-packs.toml | tr -d '\r')
  [ -n "$v" ] || { echo "deploy/language-packs.toml declares no version for [$lang]" >&2; exit 1; }
  version_fits "language pack $lang" "$v" || exit 1
  echo "$v"
}

if [ -n "$SELF_TEST" ]; then
  fails=0
  expect() { # <product> <component> <ok|refused> <why>
    local want="$3" why="$4" got=ok
    VERSION="$1"
    version_fits "self-test" "$2" >/dev/null 2>&1 || got=refused
    if [ "$got" != "$want" ]; then
      echo "self-test: product=$1 component=$2 -> $got, expected $want ($why)" >&2
      fails=$((fails + 1))
    fi
  }
  expect 0.2.0 0.2.0 ok "the ordinary case"
  expect 0.2.7 0.2.0 ok "a component unchanged for seven deliveries"
  expect 0.3.0 0.2.9 refused "off the generation"
  expect 0.2.0-beta.1 0.2.0-beta.1 ok "a beta where every component moved"
  expect 0.2.1-beta.1 0.2.0 ok "a beta shipping one component, others left behind"
  expect 0.2.1-beta1 0.2.0 ok "the same, with a suffix carrying no dot"
  expect 0.2.1-beta.1 0.2.1 refused "the number the finished release will carry"
  expect 0.2.1-beta.1 0.3.0 refused "off the generation, suffix or not"
  # A language pack is a shipped component like any other: pack_version()
  # calls this same version_fits, so these two cases are the pack-specific
  # readings of the two rules above rather than a second code path.
  expect 0.3.0 0.2.9 refused "a language pack off the product generation"
  expect 0.2.1-beta.1 0.2.1 refused "a language pack at the number the finished release will carry, inside a prerelease"
  [ "$fails" -eq 0 ] || { echo "self-test: $fails case(s) wrong" >&2; exit 1; }
  echo "self-test: version guards ok"
  exit 0
fi

stage_plugin() {
  local name="$1" dir="$2"
  mkdir -p "$dir/usr/local/lib/ritornello/plugins"
  cp "$BIN/ritornello-plugin-$name" "$dir/usr/local/lib/ritornello/plugins/"
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

# Builds one archive from a staging directory and refuses to produce
# anything unsafe to extract as root. Shared by pack() and pack_noarch(),
# which differ only in the archive's name: the owner/mode guards below are a
# SECURITY property of every archive this script builds, not formatting, so
# copying this block instead of factoring it would be two things to keep in
# step -- and the one that drifted would ship an archive with the wrong
# ownership.
_pack_archive() { # <staging dir> <archive path>
  local dir="$1" archive="$2"
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
  tar -C "$dir" --owner=root --group=root --numeric-owner --mode='u+rwX,go=rX' -czf "$archive" .
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
  rm -rf "$dir"
}

pack() { # <staging dir> <archive base name> <version>   (per-target archive)
  _pack_archive "$1" "$OUT/$2-$3-$ARCH.tar.gz"
}

pack_noarch() { # <staging dir> <archive base name> <version>
  # No `-$ARCH`: a language pack is text, built once for every architecture.
  _pack_archive "$1" "$OUT/$2-$3.tar.gz"
}

# --- one archive per language --------------------------------------------
# A pack has no architecture: it is text. It is therefore named without one,
# and built by a job of its own in the release workflow -- three per-arch
# runs producing the same file name would collide when the publish job
# merges their directories.
pack_language() { # <language>
  local lang="$1" d modules=()
  d=$(mktemp -d)
  for dir in deploy/locales/*/; do
    local module="${dir%/}"; module="${module##*/}"
    [ -f "deploy/locales/$module/$lang.toml" ] || continue
    cp "deploy/locales/$module/$lang.toml" "$d/$module.toml"
    modules+=("$module")
  done
  [ "${#modules[@]}" -gt 0 ] || { echo "no locale file for $lang" >&2; exit 1; }
  local v
  v=$(pack_version "$lang")
  {
    printf 'language = "%s"\n' "$lang"
    printf 'version = "%s"\n' "$v"
    printf 'source = "https://github.com/skerdudou/ritornello"\n'
    printf 'modules = ['
    local sep=""
    for m in "${modules[@]}"; do printf '%s"%s"' "$sep" "$m"; sep=", "; done
    printf ']\n'
  } > "$d/pack.toml"
  pack_noarch "$d" "ritornello-lang-$lang" "$v"
}

if [ -n "$LANGUAGES" ]; then
  OUT="release/languages"
  rm -rf "$OUT"
  mkdir -p "$OUT"
  mapfile -t LANGS < <(sed -n 's/^\[\(.*\)\]$/\1/p' deploy/language-packs.toml | tr -d '\r')
  [ "${#LANGS[@]}" -gt 0 ] || { echo "no language declared in deploy/language-packs.toml" >&2; exit 1; }
  for l in "${LANGS[@]}"; do pack_language "$l"; done
  ls -l "$OUT"
  echo "OK — $(ls "$OUT" | wc -l) language pack(s)"
  exit 0
fi

BIN="target/$TARGET/release"
OUT="release/$ARCH"
rm -rf "$OUT"
mkdir -p "$OUT"

# The plugin list comes from plugins.example.toml, the same source deploy.sh
# uses. Deriving it is what stops the two from diverging.
mapfile -t PLUGINS < <(sed -n 's/^name = "\(.*\)"/\1/p' deploy/plugins.example.toml | tr -d '\r')
[ "${#PLUGINS[@]}" -gt 0 ] || { echo "no plugin found in plugins.example.toml" >&2; exit 1; }

# --- the core -------------------------------------------------------------
CORE=$(mktemp -d)
mkdir -p "$CORE/usr/local/bin"
cp "$BIN/ritornello-core" "$CORE/usr/local/bin/"
# Everything else the core carries — its unit and its polkit rules — is
# named by the manifest, not repeated here. No locale catalog any more: the
# core's French comes from installing the fr language pack, like every
# other language.
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
