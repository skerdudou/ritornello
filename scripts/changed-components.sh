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
# Usage: changed-components.sh [--guard-baseline <tag>] [ref | --guard-only]
#        changed-components.sh --self-test
#   with a ref    — the components whose declared version differs from it
#   without a ref — every component (the first release, or an unknown history)
#   Either way, the coupled-change guard (run_guard) runs first, against its
#   own baseline: the newest PUBLISHED release, prereleases included, which
#   ci.yml passes as --guard-baseline ("" when none was ever published).
#   Without --guard-baseline (a local run) it falls back to git describe,
#   and says so.
#   --guard-only  — that guard alone, and its verdict as the exit code
#   --self-test   — the language-pack decision and naming, against a table of
#                    cases and against what package-release.sh actually
#                    builds, and the coupled-change guard run for real inside
#                    throwaway git repositories; see the block guarded by
#                    $SELF_TEST below. Called
#                    by the Rust suite (version_coherence.rs), for the same
#                    reason package-release.sh --self-test is: this script's
#                    only other exercise is the release job, which fires on a
#                    tag.
set -euo pipefail

cd "$(dirname "$0")/.."

SELF_TEST=
GUARD_ONLY=
PREV=
# The coupled-change guard's baseline (see run_guard below). GUARD_BASELINE_SET
# tells "given, and empty" (there has never been a published release) apart
# from "not given" (a local run, which falls back to git describe).
GUARD_BASELINE=
GUARD_BASELINE_SET=
if [ "${1:-}" = "--self-test" ]; then
  SELF_TEST=1
else
  if [ "${1:-}" = "--guard-baseline" ]; then
    [ "$#" -ge 2 ] || { echo "--guard-baseline needs a tag, or \"\" when no release was ever published" >&2; exit 1; }
    GUARD_BASELINE="$2" GUARD_BASELINE_SET=1
    shift 2
  fi
  if [ "${1:-}" = "--guard-only" ]; then
    # Runs the coupled-change guard alone (see run_guard below) and exits
    # with its verdict. What --self-test drives inside throwaway
    # repositories, so that the test exercises the real check, git and all.
    GUARD_ONLY=1
  else
    PREV="${1:-}"
    if [ -n "$PREV" ] && ! git rev-parse --verify -q "$PREV^{commit}" >/dev/null; then
      echo "$PREV is not a commit — pass a release tag, or no argument for a first release" >&2
      exit 1
    fi
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

# Companions: components that ship beside a plugin, in an archive of their
# own named after their own crate, `ritornello-<name>` (see [companions] in
# deploy/packaging.toml). One `<name> <with>` line each, from packaging.py,
# so that this script reads no TOML section with sed. Each is a component
# like the others for the loop below: published when its version moves.
#
# Through a command substitution, not `mapfile < <(...)`: a failure inside a
# process substitution escapes `set -e`, and an empty list would then mean
# "no companion" — nothing published for it, and a guard checking nothing.
COMPANIONS_TXT=$(python3 scripts/packaging.py companions)
COMPANIONS=()
[ -z "$COMPANIONS_TXT" ] || mapfile -t COMPANIONS <<< "$COMPANIONS_TXT"
if tr -d '\r' < deploy/packaging.toml | grep -q '^\[companions\.' && [ "${#COMPANIONS[@]}" -eq 0 ]; then
  echo "deploy/packaging.toml declares [companions.*] but packaging.py companions listed none" >&2
  exit 1
fi
for line in "${COMPANIONS[@]}"; do CRATES+=("ritornello-${line%% *}"); done

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

# --- the coupled-change guard ----------------------------------------------
# It CLOSES the shared-crate trap for a companion and its plugin, rather than
# warning about it. A device installs on version inequality alone, so an
# archive rebuilt with new bytes under its old number is fetched by no
# device, silently: the release looks complete and delivers part of the
# change. The rules, for each companion and the plugin its `with` names:
#
# - a change to the companion's own binary (crates/ritornello-<c>/src/bin/)
#   or to what its `tree` is built from (its unit and its rule under
#   deploy/) changes only the companion's archive: the COMPANION must move;
# - a change anywhere else in its crate (its library, its Cargo.toml beyond
#   the version line) is also linked into the plugin: BOTH must move.
#
# The version line is set aside on purpose: bumping the companion alone is
# the gesture the first rule asks for, and must not itself count as a
# library change that then demands a bump of the plugin. Only the
# `version =` line of the `[package]` table, though: a table-form dependency
# (`[dependencies.toml]` / `version = "1.1"`) states its requirement on a
# line of the same shape, and moving it is a library change.
#
# Its baseline is NOT the PREV this script is given. PREV answers "what to
# republish" and is the last stable release (see ci.yml); the guard asks
# whether each rebuilt archive carries a number the devices do not already
# run. That is the NEWEST PUBLISHED release, prereleases included (devices on
# the beta channel run it) and drafts excluded: a tag whose release the
# guard refused, or a draft abandoned with its tag left behind, was never
# delivered, and taking it as the baseline would hide the very change that
# was refused. Only GitHub knows what was published, so ci.yml asks it and
# passes the answer as --guard-baseline; "" there means no release was ever
# published, the one case with nothing to compare.
#
# Without --guard-baseline (a local run), the baseline falls back to the
# nearest `v*` tag reachable from HEAD^ (which leaves out a tag on HEAD
# itself), and the guard says it is a fallback: it cannot tell a published
# tag from a refused one. It also follows first parents only, so on a merge
# commit a tag that sits only on the merged branch is missed (R68: tags are
# placed on main after a merge, and CI passes the published baseline, which
# makes this moot there).
#
# Fail closed: a baseline that does not resolve, a HEAD^ that does not exist
# (a shallow clone), a describe that finds nothing, or a git that cannot read
# the repository, all refuse, rather than reading as "nothing to compare".

# Sets GUARD_BASE, or GUARD_BASE="" for the explicit no-published-release
# case. Returns 1, having said why, when no baseline can be trusted.
resolve_guard_baseline() {
  if [ -n "$GUARD_BASELINE_SET" ]; then
    GUARD_BASE="$GUARD_BASELINE"
    if [ -z "$GUARD_BASE" ]; then
      echo "coupled-change guard: no release has ever been published, so nothing to compare against; not checked" >&2
      return 0
    fi
    if ! git rev-parse --verify -q "$GUARD_BASE^{commit}" >/dev/null; then
      echo "coupled-change guard: the baseline $GUARD_BASE does not resolve to a commit here (a tag not fetched?); refusing" >&2
      return 1
    fi
    echo "coupled-change guard: baseline $GUARD_BASE, the newest published release" >&2
    return 0
  fi
  if ! git rev-parse --verify -q 'HEAD^{commit}' >/dev/null; then
    echo "coupled-change guard: git cannot read this repository; refusing" >&2
    return 1
  fi
  if ! git rev-parse --verify -q 'HEAD^^{commit}' >/dev/null; then
    echo "coupled-change guard: HEAD^ does not exist (a shallow clone, or a single commit); pass --guard-baseline; refusing" >&2
    return 1
  fi
  if ! GUARD_BASE=$(git describe --tags --abbrev=0 --match 'v*' 'HEAD^' 2>/dev/null) || [ -z "$GUARD_BASE" ]; then
    echo "coupled-change guard: no v* tag reachable from HEAD^ (tags not fetched?); pass --guard-baseline \"\" if no release was ever published; refusing" >&2
    return 1
  fi
  echo "coupled-change guard: LOCAL FALLBACK baseline $GUARD_BASE (git describe HEAD^; it cannot tell a published tag from a refused one, CI passes --guard-baseline)" >&2
  return 0
}

manifest_minus_version() { # <manifest text on stdin>
  tr -d '\r' | awk '/^\[/ { section = $0 } !(section == "[package]" && /^version = /)'
}

# Prints one `<crate>: <why>` line per component of the pair that should have
# moved and did not, and nothing when the pair is fine. A git failure prints
# a `GUARD-ERROR` line instead, which run_guard turns into a refusal: this
# runs inside a command substitution, where `set -e` does not reach.
coupled_problems() { # <baseline> <companion> <plugin> <companion tree source>...
  local base="$1" companion="ritornello-$2" plugin="ritornello-plugin-$3"
  shift 3
  local dir="crates/$companion" lib=no own=no f now then_ changed rc
  if ! changed=$(git diff --name-only "$base" -- "$dir"); then
    echo "GUARD-ERROR: git diff $base -- $dir failed"
    return 0
  fi
  for f in $changed; do
    case "$f" in
      "$dir"/src/bin/*) own=yes ;;
      "$dir"/Cargo.toml)
        if [ "$(git show "$base:$f" 2>/dev/null | manifest_minus_version)" != "$(manifest_minus_version < "$f")" ]; then
          lib=yes
        fi
        ;;
      *) lib=yes ;;
    esac
  done
  if [ "$#" -gt 0 ]; then
    rc=0
    git diff --quiet "$base" -- "$@" || rc=$?
    case "$rc" in
      0) ;;
      1) own=yes ;;
      *) echo "GUARD-ERROR: git diff $base -- $* failed"; return 0 ;;
    esac
  fi
  moved() { # <crate>
    now=$(version_in < "crates/$1/Cargo.toml")
    then_=$(git show "$base:crates/$1/Cargo.toml" 2>/dev/null | version_in || true)
    [ "$now" != "$then_" ]
  }
  if [ "$lib" = yes ] && ! moved "$companion"; then
    echo "$companion: its library changed ($dir outside src/bin/), and its archive would be republished under its old number"
  elif [ "$own" = yes ] && ! moved "$companion"; then
    echo "$companion: its binary, unit or rule changed, and its archive would be republished under its old number"
  fi
  if [ "$lib" = yes ] && ! moved "$plugin"; then
    echo "$plugin: it links the library of $dir, which changed, so its rebuilt binary would be published under its old number"
  fi
  return 0
}

# Returns 1, after naming every problem, when a pair did not move as it must,
# or when no baseline can be trusted.
run_guard() {
  local line name with srcs problems=""
  GUARD_BASE=
  resolve_guard_baseline || return 1
  [ -n "$GUARD_BASE" ] || return 0
  for line in "${COMPANIONS[@]}"; do
    name="${line%% *}" with="${line#* }"
    if ! srcs=$(python3 scripts/packaging.py companion-sources "$name"); then
      echo "coupled-change guard: packaging.py companion-sources $name failed" >&2
      return 1
    fi
    # shellcheck disable=SC2086 # one repository path per line, none with spaces
    problems+=$(coupled_problems "$GUARD_BASE" "$name" "$with" $srcs)
    problems+=$'\n'
  done
  problems=$(printf '%s' "$problems" | sed '/^$/d')
  # A `case`, not `printf | grep -q`: under pipefail, grep leaving early can
  # SIGPIPE the printf and turn a match into a miss.
  if case $'\n'"$problems" in *$'\n'GUARD-ERROR*) true ;; *) false ;; esac; then
    printf '%s\n' "$problems" | sed -n 's/^GUARD-ERROR: /coupled-change guard: /p' >&2
    echo "coupled-change guard: refusing, since the comparison itself failed" >&2
    return 1
  fi
  if [ -n "$problems" ]; then
    while IFS= read -r line; do
      echo "coupled-change guard: $line (since $GUARD_BASE). Bump the version in crates/${line%%:*}/Cargo.toml." >&2
    done <<< "$problems"
    return 1
  fi
  echo "coupled-change guard: every companion and its plugin moved as their changes require, since $GUARD_BASE" >&2
  return 0
}

if [ -n "$GUARD_ONLY" ]; then
  run_guard
  exit $?
fi

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

  # The coupled-change guard, through the REAL check: this very script, run
  # with --guard-only inside a throwaway git repository that holds a copy of
  # it, of packaging.py and of the manifests it reads, plus a stand-in
  # companion crate (with a table-form dependency, as a real Cargo.toml may
  # have) and plugin crate. Each case commits a baseline tagged v0.1.0, makes
  # one change, commits it and tags it v0.1.1 as a release would, then reads
  # the verdict: its exit code, which crates it names, and what it says.
  # git runs with no global or system configuration, so no hook, signing
  # rule or default of the machine can change what the test measures.
  guard_git() { GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -C "$R" -c user.name=self-test -c user.email=self-test@invalid -c init.defaultBranch=main "$@"; }
  guard_repo() { # sets R to a fresh repository whose one commit is tagged v0.1.0
    R=$(mktemp -d)
    mkdir -p "$R/scripts" "$R/deploy" "$R/crates/ritornello-files-mount/src/bin" "$R/crates/ritornello-plugin-files/src"
    cp scripts/changed-components.sh scripts/packaging.py "$R/scripts/"
    cp deploy/plugins.example.toml deploy/language-packs.toml deploy/packaging.toml "$R/deploy/"
    while IFS= read -r src; do printf 'original\n' > "$R/$src"; done < <(python3 scripts/packaging.py companion-sources files-mount)
    printf '[package]\nname = "ritornello-files-mount"\nversion = "0.1.0"\n\n[dependencies.toml]\nversion = "1.1"\n' > "$R/crates/ritornello-files-mount/Cargo.toml"
    printf 'pub fn grammar() {}\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
    printf 'fn main() {}\n' > "$R/crates/ritornello-files-mount/src/bin/media-mount.rs"
    printf '[package]\nname = "ritornello-plugin-files"\nversion = "0.1.0"\n' > "$R/crates/ritornello-plugin-files/Cargo.toml"
    printf 'fn main() {}\n' > "$R/crates/ritornello-plugin-files/src/main.rs"
    guard_git init -q
    guard_git add -A
    guard_git commit -q -m baseline
    guard_git tag v0.1.0
  }
  # Moves the [package] version only, never a dependency's.
  guard_bump() { # <crate> [version]
    local f="$R/crates/$1/Cargo.toml"
    awk -v v="${2:-0.1.1}" '/^\[/ { section = $0 } section == "[package]" && /^version = / { $0 = "version = \"" v "\"" } { print }' "$f" > "$f.tmp"
    mv "$f.tmp" "$f"
  }
  guard_release() { guard_git add -A; guard_git commit -q -m change; guard_git tag "${1:-v0.1.1}"; }
  expect_guard() { # <exit> <crates named, space-separated> <stderr must contain, or ""> <why> [args, default: --guard-baseline v0.1.0]
    local want_exit="$1" want="$2" say="$3" why="$4" got_exit=0 got
    shift 4
    # No argument: the published baseline v0.1.0. A lone `--`: none at all,
    # the local fallback.
    if [ "$#" -eq 0 ]; then set -- --guard-baseline v0.1.0; elif [ "$1" = "--" ]; then shift; fi
    bash "$R/scripts/changed-components.sh" "$@" --guard-only > "$R.out" 2> "$R.err" || got_exit=$?
    got=$(sed -n 's/^coupled-change guard: \(ritornello-[a-z-]*\): .*/\1/p' "$R.err" | sort | tr '\n' ' ')
    got="${got% }"
    if [ "$got_exit" != "$want_exit" ] || [ "$got" != "$want" ] || { [ -n "$say" ] && ! grep -qF -- "$say" "$R.err"; }; then
      echo "self-test: coupled guard [$*] -> exit $got_exit naming [$got], expected exit $want_exit naming [$want]${say:+ saying \"$say\"} ($why)" >&2
      sed 's/^/    /' "$R.err" >&2
      fails=$((fails + 1))
    fi
    rm -rf "$R" "$R.out" "$R.err"
  }

  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_bump ritornello-files-mount; guard_bump ritornello-plugin-files; guard_release
  expect_guard 0 "" "" "library changed, both moved"

  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_bump ritornello-files-mount; guard_release
  expect_guard 1 "ritornello-plugin-files" "" "library changed, only the companion moved: the plugin links it"

  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_bump ritornello-plugin-files; guard_release
  expect_guard 1 "ritornello-files-mount" "" "library changed, only the plugin moved"

  guard_repo
  printf 'fn main() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/bin/media-mount.rs"
  guard_bump ritornello-files-mount; guard_release
  expect_guard 0 "" "" "binary-only change with the companion moved: files is not forced to move"

  guard_repo
  printf 'fn main() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/bin/media-mount.rs"
  guard_release
  expect_guard 1 "ritornello-files-mount" "" "binary-only change, nothing moved"

  guard_repo
  printf 'changed\n' > "$R/deploy/ritornello-media-mount.service"
  guard_release
  expect_guard 1 "ritornello-files-mount" "" "the companion's unit changed without a bump"

  guard_repo
  sed 's/^version = "1.1"$/version = "1.2"/' "$R/crates/ritornello-files-mount/Cargo.toml" > "$R/m" && mv "$R/m" "$R/crates/ritornello-files-mount/Cargo.toml"
  guard_bump ritornello-files-mount; guard_release
  expect_guard 1 "ritornello-plugin-files" "" "a table-form dependency's version moved: a library change, not the [package] version line"

  # Repo D of the re-review: v0.1.1 is refused and never published, then an
  # unrelated commit is tagged v0.1.2. Passed the newest PUBLISHED release
  # (still v0.1.0), the guard must still refuse; the refused tag is not a
  # baseline.
  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_bump ritornello-files-mount; guard_release v0.1.1
  printf 'unrelated\n' > "$R/README"; guard_release v0.1.2
  expect_guard 1 "ritornello-plugin-files" "since v0.1.0" "a refused tag v0.1.1 must not become the baseline once the published one is passed in"

  # The local fallback: no --guard-baseline, git describe HEAD^, said so.
  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_bump ritornello-files-mount; guard_release
  expect_guard 1 "ritornello-plugin-files" "LOCAL FALLBACK baseline v0.1.0" "the local fallback finds v0.1.0 and says it is a fallback" --

  # The one case that passes unchecked: told that nothing was ever published.
  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_release
  expect_guard 0 "" "no release has ever been published" "an explicit empty baseline: nothing was ever published" --guard-baseline ""

  # Fail closed.
  guard_repo
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_release
  expect_guard 1 "" "does not resolve" "a baseline that does not resolve refuses" --guard-baseline v9.9.9

  guard_repo
  expect_guard 1 "" "HEAD^ does not exist" "local fallback with no HEAD^ (a shallow clone) refuses" --

  guard_repo
  guard_git tag -d v0.1.0 >/dev/null
  printf 'pub fn grammar() { /* v2 */ }\n' > "$R/crates/ritornello-files-mount/src/lib.rs"
  guard_release
  expect_guard 1 "" "no v* tag reachable from HEAD^" "local fallback where describe finds nothing refuses" --

  guard_repo
  rm -rf "$R/.git"
  expect_guard 1 "" "git cannot read this repository" "local fallback outside any repository refuses" --

  # The pairs the real check walks, read from packaging.toml.
  found_pair=no
  for line in "${COMPANIONS[@]}"; do
    if [ "$line" = "files-mount files" ]; then found_pair=yes; fi
  done
  if [ "$found_pair" = no ]; then
    echo "self-test: packaging.py companions does not list 'files-mount files': the coupled guard would check nothing" >&2
    fails=$((fails + 1))
  fi

  [ "$fails" -eq 0 ] || { echo "self-test: $fails case(s) wrong" >&2; exit 1; }
  echo "self-test: language-pack change detection and the coupled-change guard ok"
  exit 0
fi

# The coupled-change guard (see run_guard above), against its own baseline.
# Checked before anything is printed, so a refused release leaves no half
# list on stdout.
run_guard || exit 1

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
