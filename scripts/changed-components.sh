#!/usr/bin/env bash
# Which shipped components changed since a given git ref, one archive base
# name per line.
#
# It lives here rather than inside the workflow YAML for the same reason
# package-release.sh does: it must be runnable on a development machine and its
# output looked at, instead of being discovered by pushing a tag. The release
# workflow has never run in this repository, so a decision buried in YAML would
# be a decision nobody has ever seen taken.
#
# The rule: a component is published when the version it declares differs from
# the version it declared at <ref>. Bumping the number is therefore the only
# gesture, and forgetting to bump publishes nothing — a loud failure rather
# than a silent one.
#
# Usage: changed-components.sh [ref] | --self-test
#   with a ref    — the components whose declared version differs from it
#   without a ref — every component (the first release, or an unknown history)
#   --self-test   — the language-pack decision and naming, against a table of
#                    cases and against what package-release.sh actually
#                    builds; see the block guarded by $SELF_TEST below. Called
#                    by the Rust suite (version_coherence.rs), for the same
#                    reason package-release.sh --self-test is: this script's
#                    only other exercise is the release job, which fires on a
#                    tag.
set -euo pipefail

cd "$(dirname "$0")/.."

SELF_TEST=
PREV=
if [ "${1:-}" = "--self-test" ]; then
  SELF_TEST=1
else
  PREV="${1:-}"
  if [ -n "$PREV" ] && ! git rev-parse --verify -q "$PREV^{commit}" >/dev/null; then
    echo "$PREV is not a commit — pass a release tag, or no argument for a first release" >&2
    exit 1
  fi
fi

# Shared crates are linked into every component's binary and inherit the
# product version, so changing one moves no declared number while rebuilding
# all eleven. Publishing "what changed" would then leave ten plugins on the
# device built against the old crate, with version equality claiming
# everything is up to date. A change to any of them therefore counts as a
# change to everything — decided by content, not by a number.
#
# **Republishing is not, by itself, delivering**, and that limit is real: the
# archives go out under the components' UNCHANGED versions, and the device
# decides what to install with `release::differs`, which is plain version
# inequality. Equal versions read as "up to date", so not one of those
# rebuilt archives is ever fetched. A shared-crate change reaches devices
# only if **every** component's version is bumped by hand in the same commit.
# This script cannot do that for you and does not refuse the release: it says
# so on stderr, `docs/installation.md` says so, and the release-notes
# template asks for it.
#
# Not detected, and assumed: an external dependency bump lives in Cargo.lock,
# which moves whenever any version moves. Detecting it would republish
# everything at every delivery and defeat the point. If a dependency bump
# matters, bumping every component is the gesture.
#
# This list is the same four crates as INTERNAL_CRATES in
# version_coherence.rs — not derived from it (there is no manifest either
# side can read the other from without more machinery than four entries
# deserve), so a crate added to one belongs in the other too.
#
# ritornello-updater is not linked into anything, but its binary ships inside
# the core's archive (deploy/packaging.toml, extra_binaries), so changing it
# changes what that archive carries while moving no declared version. Strictly
# it affects only the core; it is listed here because over-publishing is the
# safe direction and one mechanism is better than two for a crate that changes
# rarely.
SHARED=(crates/ritornello-proto crates/ritornello-i18n crates/ritornello-plugin-sdk crates/ritornello-updater)
if [ -n "$PREV" ] && ! git diff --quiet "$PREV" -- "${SHARED[@]}"; then
  echo "a shared crate changed since $PREV — every component is published" >&2
  echo "  NOTE: republished archives keep their unchanged version numbers, and a device" >&2
  echo "  installs on version inequality alone — so no device will fetch any of them." >&2
  echo "  For a shared-crate change to reach devices, bump EVERY component's version." >&2
  PREV=""
fi

# The plugin list comes from plugins.example.toml, the same source
# package-release.sh and deploy.sh derive it from. Deriving it is what stops
# the three from diverging. `tr -d '\r'` for the same CRLF reason as there:
# .gitattributes does not normalize *.toml.
mapfile -t PLUGINS < <(sed -n 's/^name = "\(.*\)"/\1/p' deploy/plugins.example.toml | tr -d '\r')
[ "${#PLUGINS[@]}" -gt 0 ] || { echo "no plugin found in plugins.example.toml" >&2; exit 1; }

CRATES=(ritornello-core)
for p in "${PLUGINS[@]}"; do CRATES+=("ritornello-plugin-$p"); done

# The version a manifest declares, or the literal `inherited` when it uses
# `version.workspace = true`. Two distinct answers, because a component that
# inherits is a component from before this scheme: it must count as changed so
# the first release under the scheme publishes everything.
version_in() { # <manifest text on stdin>
  local v
  v=$(tr -d '\r' | sed -n -e 's/^version = "\(.*\)"/\1/p' -e 's/^version\.workspace = true$/inherited/p' | head -1)
  [ -n "$v" ] || v=absent
  printf '%s\n' "$v"
}

# Language packs: one version per `[language]` section of
# deploy/language-packs.toml, which is the file's only home for that number
# -- a pack has no Cargo.toml. The languages come from that same file, the
# same way the plugin list above comes from plugins.example.toml. Defined
# here, ahead of the CRATES loop, so --self-test can exercise these
# functions below without running any of the git-dependent logic above.
# tr BEFORE sed, not after: the pattern is anchored on `]$`, and a
# CRLF-terminated file (the Windows-checkout case package-release.sh's own
# comment warns about) leaves a trailing \r that defeats that anchor before
# tr ever runs on the -- by then empty -- output.
mapfile -t LANGS < <(tr -d '\r' < deploy/language-packs.toml | sed -n 's/^\[\(.*\)\]$/\1/p')
[ "${#LANGS[@]}" -gt 0 ] || { echo "no language declared in deploy/language-packs.toml" >&2; exit 1; }

# The version declared for one language section, from a language-packs.toml
# on stdin. Scoped by hand, like pack_version() in package-release.sh: the
# file holds one section per language, and a plain sed over the whole file
# would answer with whichever language's `version =` line came first. `tr`
# runs on stdin before awk, for the same anchored-`$0 ==` reason as LANGS
# above -- awk's exact-match section header would otherwise never see a
# CRLF-terminated `[fr]` as equal to the literal `[fr]` it is looking for.
pack_version() { # <language>
  local lang="$1" v
  v=$(tr -d '\r' | awk -v section="[$lang]" '
    $0 == section { found=1; next }
    found && /^\[/ { found=0 }
    found && /^version = "/ { sub(/^version = "/, ""); sub(/"$/, ""); print; exit }
  ')
  [ -n "$v" ] || v=absent
  printf '%s\n' "$v"
}

# Whether a pack counts as changed: its declared version now differs from
# what it declared at the reference (or from "absent", when there is no
# reference or the pack is new there). Factored out so --self-test exercises
# the SAME comparison the real loop below makes, rather than a restatement
# of it -- the same reason version_fits is its own function in
# package-release.sh instead of being inlined at both call sites.
pack_changed() { # <now> <then>
  [ "$1" != "$2" ]
}

# The name a filter downstream matches on: the `publish` job of ci.yml keeps
# only `assets/"$c"-*.tar.gz` for every name this script prints, so this must
# be `ritornello-lang-<language>` with no version -- package-release.sh names
# the archive itself `ritornello-lang-<language>-<version>.tar.gz`, and the
# filter appends `-*.tar.gz` on its own. A name that already carried the
# version would match nothing, and the archive would be built and then
# silently discarded by that job's `rm -rf assets`. Factored into its own
# function for the same reason as pack_changed above: the real loop below and
# --self-test's cross-check against package-release.sh must call the exact
# same code, not two copies of the string "ritornello-lang-" that could drift
# apart from each other.
pack_archive_name() { # <language>
  printf 'ritornello-lang-%s\n' "$1"
}

if [ -n "$SELF_TEST" ]; then
  fails=0

  # The positive and negative halves of the decision the per-language loop
  # below makes. A real git ref cannot exercise both in isolation: the one
  # tag this repository has predates deploy/language-packs.toml entirely, so
  # comparing against it can only ever land on the "new since the reference"
  # branch or the shared-crate fallback that republishes everything --
  # never a case where the file existed at both ends and the version simply
  # did, or did not, move. See task-12-report.md for the measurements.
  expect_changed() { # <now> <then> <yes|no> <why>
    local now="$1" then_="$2" want="$3" why="$4" got=no
    pack_changed "$now" "$then_" && got=yes
    if [ "$got" != "$want" ]; then
      echo "self-test: now=$now then=$then_ -> $got, expected $want ($why)" >&2
      fails=$((fails + 1))
    fi
  }
  expect_changed 0.2.1-beta.2 0.2.0-beta.2 yes "the ordinary case: a pack whose number moved"
  expect_changed 0.2.0-beta.2 0.2.0-beta.2 no  "a pack whose number did not move -- publishing it would look like a release, and deliver nothing"
  expect_changed 0.2.0-beta.2 absent       yes "a pack new since the reference"

  # The naming half, pinned against package-release.sh's OWN archive-building
  # rather than a second copy of the same literal: this actually builds the
  # language packs the way the release workflow does (--languages needs no
  # toolchain, only deploy/locales and tar) and checks that the file it
  # names exists at the exact path pack_archive_name() plus the real version
  # would predict. If either script's naming ever drifted from the other,
  # the expected file would not exist, and the publish job's filter would
  # silently drop the archive it built.
  rm -rf release/languages
  bash scripts/package-release.sh --languages >/dev/null
  for l in "${LANGS[@]}"; do
    v=$(pack_version "$l" < deploy/language-packs.toml)
    archive="release/languages/$(pack_archive_name "$l")-$v.tar.gz"
    if [ ! -f "$archive" ]; then
      echo "self-test: this script would emit '$(pack_archive_name "$l")' for [$l], but package-release.sh built no archive at $archive -- the publish job's filter (assets/\"\$c\"-*.tar.gz) would match nothing" >&2
      fails=$((fails + 1))
    fi
  done
  rm -rf release/languages

  [ "$fails" -eq 0 ] || { echo "self-test: $fails case(s) wrong" >&2; exit 1; }
  echo "self-test: language-pack change detection ok"
  exit 0
fi

changed=0
for c in "${CRATES[@]}"; do
  now=$(version_in < "crates/$c/Cargo.toml")
  [ "$now" != absent ] || { echo "crates/$c declares no version" >&2; exit 1; }
  if [ -z "$PREV" ]; then
    printf '%s\n' "$c"
    changed=$((changed + 1))
    continue
  fi
  # A crate that did not exist at <ref> is new, so it counts as changed. `git
  # show` failing for any other reason would be indistinguishable here, which
  # is acceptable: the fallback publishes the component instead of skipping it.
  then_=$(git show "$PREV:crates/$c/Cargo.toml" 2>/dev/null | version_in || echo absent)
  if [ "$now" != "$then_" ]; then
    printf '%s\n' "$c"
    changed=$((changed + 1))
  fi
done

# Language packs: LANGS, pack_version(), pack_changed() and
# pack_archive_name() are all defined above, ahead of the CRATES loop, so
# --self-test can reach them without the git-dependent logic in between.
MOVED_PACKS=()
for l in "${LANGS[@]}"; do
  now=$(pack_version "$l" < deploy/language-packs.toml)
  [ "$now" != absent ] || { echo "deploy/language-packs.toml declares no version for [$l]" >&2; exit 1; }
  if [ -z "$PREV" ]; then
    then_=absent
  else
    # A language that did not exist at <ref> is new, so it counts as
    # changed -- same fallback as the crate loop above, for the same reason.
    then_=$(git show "$PREV:deploy/language-packs.toml" 2>/dev/null | pack_version "$l" || echo absent)
  fi
  if pack_changed "$now" "$then_"; then
    printf '%s\n' "$(pack_archive_name "$l")"
    MOVED_PACKS+=("$(pack_archive_name "$l")")
    changed=$((changed + 1))
  fi
done

# A text changed with no number moved delivers nothing, and looks exactly
# like a release that worked. Same class as the shared-crate case above, and
# said the same way: on stderr, without refusing the release.
if [ -n "$PREV" ] && ! git diff --quiet "$PREV" -- deploy/locales; then
  if ! printf '%s\n' "${MOVED_PACKS[@]}" | grep -q .; then
    echo "deploy/locales changed since $PREV but no language pack version moved —" >&2
    echo "  the new text will not reach any device. Bump the pack in deploy/language-packs.toml." >&2
  fi
fi

if [ "$changed" -eq 0 ]; then
  echo "no component version moved since $PREV — bump the component you fixed, or this release delivers nothing" >&2
  exit 2
fi
