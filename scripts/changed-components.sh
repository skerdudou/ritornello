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
# than a silent one. Three things ask for more than a component's own
# move: a change of `PROTOCOL_VERSION` (the bootstrap wire break: old binaries
# can no longer talk to the core, so every core and plugin must have moved, and
# the script refuses the release otherwise); a change of one wire contract's
# MAJOR (crates/ritornello-proto/src/contract.rs: the core and the plugins
# that speak that contract must have moved, and the script refuses the release
# otherwise — a plugin that does not speak it is left alone); and a change of
# the product's MAJOR, which republishes everything. A COMPATIBLE change to a
# shared crate republishes nothing by itself: see the block below.
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

# Shared crates (proto, i18n, the plugin SDK, the updater) are linked into
# the binaries and inherit the product version, so changing one moves no
# declared number. That used to republish every component, and a device
# installs on version inequality alone, so those archives were fetched by
# nobody. The rule now is the owner's: DO NOT republish needlessly. A
# component whose current binary still works with the new core stays where it
# is. A component is republished only when
#   1. its own version moved (its code changed, as always);
#   2. `PROTOCOL_VERSION` changed (the bootstrap wire break): old binaries
#      cannot talk to the core any more, so every shipped component that links
#      `ritornello-proto` (the core and the plugins; NOT a companion, which
#      depends on no shared crate, and NOT a language pack, which is data)
#      MUST have moved its version, or the release is refused below;
#   3. one wire contract's major changed: the core and the plugins that speak
#      that contract (their `kinds` and `admin` declaration) MUST have moved,
#      or the release is refused below; the others are not asked to;
#   4. the product's major changed.
# A compatible shared-crate change prints a NOTE and nothing else: if the fix
# must reach plugins, the developer bumps those plugins by hand, which is a
# delivery choice and not a compatibility matter. The fingerprint test of
# `ritornello-proto` is what forces the break-or-compatible decision on
# whoever touches the wire.
#
# Not detected, and assumed: an external dependency bump lives in Cargo.lock,
# which moves whenever any version moves.
#
# This list is the same four crates as INTERNAL_CRATES in
# version_coherence.rs — not derived from it (there is no manifest either
# side can read the other from without more machinery than four entries
# deserve), so a crate added to one belongs in the other too.
SHARED=(crates/ritornello-proto crates/ritornello-i18n crates/ritornello-plugin-sdk crates/ritornello-updater)

# The PROTOCOL_VERSION a ritornello-proto lib.rs declares, from stdin, or
# `absent`.
proto_version() {
  local v
  v=$(tr -d '\r' | sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9][0-9]*\);.*/\1/p' | head -1)
  [ -n "$v" ] || v=absent
  printf '%s\n' "$v"
}

# The version of one wire contract, "<major>.<minor>", from a contract.rs on
# stdin, or `absent`. The constants keep a one-line shape on purpose (see
# crates/ritornello-proto/src/contract.rs).
contract_version() { # <CONST_NAME>
  local v
  v=$(tr -d '\r' | sed -n "s/^pub const $1: ContractVersion = ContractVersion::new(\([0-9][0-9]*\), *\([0-9][0-9]*\));.*/\1.\2/p" | head -1)
  [ -n "$v" ] || v=absent
  printf '%s\n' "$v"
}
# Written by hand, so --self-test holds it to the `pub const *_CONTRACT`
# lines of contract.rs: a contract added there and forgotten here would never
# be checked for its speakers.
CONTRACTS=(source:SOURCE_CONTRACT display:DISPLAY_CONTRACT input:INPUT_CONTRACT metadata:METADATA_CONTRACT admin:ADMIN_CONTRACT)

# The wire fingerprint fixture (crates/ritornello-proto/tests/wire_fingerprint.rs).
# Its contract headers read `[<name> <major>.<minor>]`, or carry a trailing
# `next` while that version has been in no release: the test lets such a
# section change in place, since it cannot see git. This script can, so the
# release is where the mark is held to the truth (run_wire_guard below).
WIRE_FIXTURE=crates/ritornello-proto/tests/wire-fingerprint.txt

# The header line of contract section <name>, from a fixture on stdin, or nothing.
wire_header() { # <name>
  tr -d '\r' | awk -v n="$1" '/^\[/ { split(substr($0, 2, length($0) - 2), w, " "); if (w[1] == n) { print; exit } }'
}
# The sample lines of contract section <name>, from a fixture on stdin.
wire_lines() { # <name>
  tr -d '\r' | awk -v n="$1" '/^\[/ { split(substr($0, 2, length($0) - 2), w, " "); on = (w[1] == n); next } on'
}
# Word <i> of a header: 2 is the version, 3 the mark.
header_word() { # <header> <i>
  printf '%s\n' "$1" | tr -d '[]' | awk -v i="$2" '{ print $i }'
}

# Does plugin crate <c> speak contract <name>? Read, parsed, from its
# declaration (the same one plugin_catalogue_declaration.rs holds to what the
# binary registers) by packaging.py, so that no TOML is read with sed. Status
# 0 speaks, 3 does not. ANY other status (a crash is 1, a manifest packaging.py
# could not read is 2, a missing python is 127) aborts the release: a plugin
# that could not be read must never be taken for one that is not required.
speaks() { # <crate> <contract>
  local rc=0
  python3 scripts/packaging.py speaks "$1" "$2" || rc=$?
  case "$rc" in
    0) return 0 ;;
    3) return 1 ;;
    *) echo "cannot tell whether $1 speaks the $2 contract (packaging.py exited $rc): release refused" >&2; exit 1 ;;
  esac
}

# The product's major, from a workspace Cargo.toml on stdin: the first
# `version = "..."` line, the same one package-release.sh reads.
product_major() {
  local v
  v=$(tr -d '\r' | sed -n 's/^version = "\([0-9][0-9]*\)\..*/\1/p' | head -1)
  [ -n "$v" ] || v=absent
  printf '%s\n' "$v"
}

# ALL is set when this release republishes every core and plugin regardless of
# its version: the first release (no PREV, handled below) is the other way
# to the same result. PROTO_BREAK is set when the wire changed.
ALL=
PROTO_BREAK=
# MAJOR_MOVED is the only trigger, besides the lack of a PREV, that also
# republishes the language packs: a wire break does not touch them, they are
# data and embed no proto.
MAJOR_MOVED=
if [ -n "$PREV" ]; then
  now_proto=$(proto_version < crates/ritornello-proto/src/lib.rs)
  # Fail loudly: a PROTOCOL_VERSION this script cannot read would make every
  # wire break invisible, and the release would look complete.
  if [ "$now_proto" = absent ]; then
    echo "cannot read PROTOCOL_VERSION from crates/ritornello-proto/src/lib.rs" >&2
    exit 1
  fi
  then_proto=$(git show "$PREV:crates/ritornello-proto/src/lib.rs" 2>/dev/null | proto_version || echo absent)
  now_major=$(product_major < Cargo.toml)
  then_major=$(git show "$PREV:Cargo.toml" 2>/dev/null | product_major || echo absent)
  if [ "$now_proto" != "$then_proto" ]; then
    PROTO_BREAK=1 ALL=1
    echo "PROTOCOL_VERSION moved ($then_proto -> $now_proto) since $PREV — every component is republished" >&2
  fi
  if [ "$now_major" != "$then_major" ]; then
    ALL=1 MAJOR_MOVED=1
    echo "the product's major moved ($then_major -> $now_major) since $PREV — every component is republished" >&2
  fi
  if [ -z "$ALL" ] && ! git diff --quiet "$PREV" -- "${SHARED[@]}"; then
    echo "NOTE: a shared crate changed since $PREV; it changed compatibly, so components are not republished" >&2
    echo "  unless their version moved — bump a plugin by hand if a fix must reach it." >&2
  fi
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
COMPANION_CRATES=()
for line in "${COMPANIONS[@]}"; do CRATES+=("ritornello-${line%% *}"); COMPANION_CRATES+=("ritornello-${line%% *}"); done
is_companion() { # <crate>
  local c
  for c in "${COMPANION_CRATES[@]}"; do [ "$c" = "$1" ] && return 0; done
  return 1
}

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

# --- the wire guard -----------------------------------------------------------
# The fingerprint test lets a contract section marked `next` change without
# its version moving: the owner's rule, an unpublished version is completed,
# not bumped. Three things only this script can check make that safe:
#
# - the release publishes every section, so none may still carry the mark:
#   after it, a change must bump;
# - a section changed under the version the newest published release (the
#   coupled guard's baseline, prereleases included: devices on the beta channel
#   run it) already carried is refused, mark or not. That is what makes the
#   mark impossible to forge: written by hand over a published section, it
#   passes the test and stops here;
# - a version lower than the one the baseline published is refused: a version
#   never goes down, and the test, blind to git, cannot see it went down.
#
# A baseline without the fixture, or without that section (a release older
# than the contracts), has nothing to compare. No baseline at all (nothing ever
# published) checks the mark only.
run_wire_guard() {
  local entry cname now_h base_h now_v base_v listing="" base_text="" have_base="" problems=()
  if [ ! -f "$WIRE_FIXTURE" ]; then
    echo "wire guard: $WIRE_FIXTURE is missing; refusing" >&2
    return 1
  fi
  # Absent and unreadable are two answers: `git cat-file -e` gives the same
  # status for both, and read as "absent" a failing git would wave every
  # published section through. ls-tree fails on a git failure and prints
  # nothing for a tree that lacks the file.
  if [ -n "$GUARD_BASE" ]; then
    if ! listing=$(git ls-tree --name-only "$GUARD_BASE" -- "$WIRE_FIXTURE"); then
      echo "wire guard: git ls-tree $GUARD_BASE -- $WIRE_FIXTURE failed; refusing" >&2
      return 1
    fi
  fi
  if [ -n "$GUARD_BASE" ] && [ -n "$listing" ]; then
    if ! base_text=$(git show "$GUARD_BASE:$WIRE_FIXTURE"); then
      echo "wire guard: git show $GUARD_BASE:$WIRE_FIXTURE failed; refusing" >&2
      return 1
    fi
    have_base=1
  fi
  for entry in "${CONTRACTS[@]}"; do
    cname=${entry%%:*}
    now_h=$(wire_header "$cname" < "$WIRE_FIXTURE")
    if [ -z "$now_h" ]; then
      problems+=("$cname: $WIRE_FIXTURE has no section for it")
      continue
    fi
    if [ "$(header_word "$now_h" 3)" = next ]; then
      problems+=("$cname: its section is still marked next ($now_h); this release publishes it, so remove the mark in the release preparation")
    fi
    [ -n "$have_base" ] || continue
    base_h=$(printf '%s\n' "$base_text" | wire_header "$cname")
    [ -n "$base_h" ] || continue
    now_v=$(header_word "$now_h" 2) base_v=$(header_word "$base_h" 2)
    if ! [[ "$now_v" =~ ^[0-9]+\.[0-9]+$ && "$base_v" =~ ^[0-9]+\.[0-9]+$ ]]; then
      problems+=("$cname: cannot compare its version $now_v with $base_v published by $GUARD_BASE")
      continue
    fi
    # A version never goes down (docs/development.md). The fingerprint test
    # cannot see git, so it cannot tell a lowered version from a new one: the
    # release is the one place that can.
    if (( 10#${now_v%%.*} < 10#${base_v%%.*} \
      || (10#${now_v%%.*} == 10#${base_v%%.*} && 10#${now_v#*.} < 10#${base_v#*.}) )); then
      problems+=("$cname: its version $now_v is lower than $base_v published by $GUARD_BASE; a version never goes down")
      continue
    fi
    if [ "$now_v" = "$base_v" ] \
      && [ "$(wire_lines "$cname" < "$WIRE_FIXTURE")" != "$(printf '%s\n' "$base_text" | wire_lines "$cname")" ]; then
      problems+=("$cname: its messages changed since $GUARD_BASE but its version $(header_word "$now_h" 2), which $GUARD_BASE published, did not move; bump ${entry#*:} in crates/ritornello-proto/src/contract.rs")
    fi
  done
  if [ "${#problems[@]}" -gt 0 ]; then
    printf 'wire guard: %s\n' "${problems[@]}" >&2
    return 1
  fi
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

  # The republication rule, through the REAL script: a throwaway repository
  # holding every crate the script lists, a proto crate carrying
  # PROTOCOL_VERSION, one more shared crate and a root Cargo.toml. Each case
  # commits a baseline tagged v0.1.0, changes things, commits, and runs the
  # whole script against v0.1.0 with no published baseline for the coupled
  # guard (--guard-baseline "").
  # The declarations of the real plugin crates, read before any cd into a
  # temporary repository. Companions and the core carry none.
  CONTRACT_NAMES=(SOURCE DISPLAY INPUT METADATA ADMIN)
  declare -A REAL_KINDS REAL_ADMIN
  for c in "${CRATES[@]}"; do
    k=$(tr -d '\r' < "crates/$c/Cargo.toml" | grep '^kinds = ' || true)
    [ -n "$k" ] || continue
    REAL_KINDS[$c]=$k
    REAL_ADMIN[$c]=$(tr -d '\r' < "crates/$c/Cargo.toml" | grep -x 'admin = true' || true)
    [ -z "${REAL_ADMIN[$c]}" ] || REAL_ADMIN[$c]+=$'\n'
  done
  rel_repo() { # sets R
    R=$(mktemp -d)
    mkdir -p "$R/scripts" "$R/deploy" "$R/crates/ritornello-proto/src" "$R/crates/ritornello-i18n/src"
    cp scripts/changed-components.sh scripts/packaging.py "$R/scripts/"
    cp deploy/plugins.example.toml deploy/language-packs.toml deploy/packaging.toml "$R/deploy/"
    printf '[workspace.package]\nversion = "0.1.0"\n' > "$R/Cargo.toml"
    printf 'pub const PROTOCOL_VERSION: u32 = 1;\n' > "$R/crates/ritornello-proto/src/lib.rs"
    printf 'pub fn t() {}\n' > "$R/crates/ritornello-i18n/src/lib.rs"
    for k in "${CONTRACT_NAMES[@]}"; do
      printf 'pub const %s_CONTRACT: ContractVersion = ContractVersion::new(1, 0);\n' "$k" >> "$R/crates/ritornello-proto/src/contract.rs"
    done
    mkdir -p "$R/crates/ritornello-proto/tests"
    for k in "${CONTRACT_NAMES[@]}"; do
      printf '[%s 1.0]\nSample = %s\n' "$(printf '%s' "$k" | tr 'A-Z' 'a-z')" "$k" >> "$R/crates/ritornello-proto/tests/wire-fingerprint.txt"
    done
    for c in "${CRATES[@]}"; do
      mkdir -p "$R/crates/$c"
      printf '[package]\nname = "%s"\nversion = "0.1.0"\n' "$c" > "$R/crates/$c/Cargo.toml"
      # A plugin stub carries the REAL crate's declaration (kinds, admin), so
      # the contract rule is exercised against what each plugin really speaks.
      if [ -n "${REAL_KINDS[$c]:-}" ]; then
        printf '\n[package.metadata.ritornello]\n%s\n%s' "${REAL_KINDS[$c]}" "${REAL_ADMIN[$c]}" >> "$R/crates/$c/Cargo.toml"
      fi
    done
    guard_git init -q; guard_git add -A; guard_git commit -q -m baseline; guard_git tag v0.1.0
  }
  rel_bump() { guard_bump "$1" "${2:-0.1.1}"; }
  # Sets one contract's version in the temporary repository: <NAME> <M, N>.
  contract_set() {
    local f="$R/crates/ritornello-proto/src/contract.rs"
    sed "s/^\(pub const $1_CONTRACT: ContractVersion = ContractVersion::new(\)[0-9]*, [0-9]*/\1$2/" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
  }
  # Rewrites the baseline so that v0.1.0 holds no contract.rs, then restores
  # the file for the case: contract.rs absent at PREV, present now.
  # With an argument, the baseline holds that text instead: a contract.rs
  # that exists at PREV and cannot be read.
  git_rm_baseline_contract() { # [baseline text]
    cp "$R/crates/ritornello-proto/src/contract.rs" "$R/contract.keep"
    if [ "$#" -gt 0 ]; then printf '%s
' "$1" > "$R/crates/ritornello-proto/src/contract.rs"; else rm "$R/crates/ritornello-proto/src/contract.rs"; fi
    guard_git add -A; guard_git commit -q -m "baseline without contracts"; guard_git tag -f v0.1.0 > /dev/null
    mv "$R/contract.keep" "$R/crates/ritornello-proto/src/contract.rs"
  }
  # Moves one language pack's version in the repository's own copy.
  pack_bump() { # <language>
    tr -d '\r' < "$R/deploy/language-packs.toml" \
      | awk -v sec="[$1]" '$0 == sec { f = 1 } f && /^version = / { $0 = "version = \"9.9.9\""; f = 0 } { print }' \
      > "$R/m" && mv "$R/m" "$R/deploy/language-packs.toml"
  }
  expect_rel() { # <exit> <stdout lines, space-separated, sorted> <stderr must contain, or ""> <why> [args]
    local want_exit="$1" want="$2" say="$3" why="$4" got_exit=0 got
    shift 4
    if [ "$#" -eq 0 ]; then set -- --guard-baseline "" v0.1.0; fi
    guard_git add -A; guard_git commit -q -m change --allow-empty
    bash "$R/scripts/changed-components.sh" "$@" > "$R.out" 2> "$R.err" || got_exit=$?
    if [ -n "${KEEP_PACKS:-}" ]; then
      got=$(sort "$R.out" | tr '\n' ' ')
    else
      got=$({ grep -v '^ritornello-lang-' "$R.out" || true; } | sort | tr '\n' ' ')
    fi
    got="${got% }"
    if [ "$got_exit" != "$want_exit" ] || [ "$got" != "$want" ] || { [ -n "$say" ] && ! grep -qF -- "$say" "$R.err"; }; then
      echo "self-test: republication [$*] -> exit $got_exit printing [$got], expected exit $want_exit printing [$want]${say:+ saying \"$say\"} ($why)" >&2
      sed 's/^/    /' "$R.err" >&2
      fails=$((fails + 1))
    fi
    rm -rf "$R" "$R.out" "$R.err"
  }
  non_companions=()
  for c in "${CRATES[@]}"; do is_companion "$c" || non_companions+=("$c"); done
  all_non_companions=$(printf '%s\n' "${non_companions[@]}" | sort | tr '\n' ' '); all_non_companions="${all_non_companions% }"
  pack_names=()
  for l in "${LANGS[@]}"; do pack_names+=("$(pack_archive_name "$l")"); done
  all_with_packs=$(printf '%s\n' "${non_companions[@]}" "${pack_names[@]}" | sort | tr '\n' ' '); all_with_packs="${all_with_packs% }"
  all_crates=$(printf '%s\n' "${CRATES[@]}" | sort | tr '\n' ' '); all_crates="${all_crates% }"

  rel_repo
  printf 'pub const PROTOCOL_VERSION: u32 = 1;\npub fn extra() {}\n' > "$R/crates/ritornello-proto/src/lib.rs"
  expect_rel 2 "" "changed compatibly" "a compatible shared-crate change, nothing moved: nothing republished, a note"

  rel_repo
  printf 'pub fn t() { /* compatible */ }\n' > "$R/crates/ritornello-i18n/src/lib.rs"
  rel_bump ritornello-plugin-radio
  expect_rel 0 "ritornello-plugin-radio" "changed compatibly" "a compatible shared-crate change plus one moved plugin: only that plugin"

  rel_repo
  printf 'pub const PROTOCOL_VERSION: u32 = 2;\n' > "$R/crates/ritornello-proto/src/lib.rs"
  for c in "${non_companions[@]}"; do rel_bump "$c"; done
  expect_rel 0 "$all_non_companions" "PROTOCOL_VERSION moved" "a wire break with every core and plugin moved: all republished, the companion not"

  rel_repo
  printf 'pub const PROTOCOL_VERSION: u32 = 2;\n' > "$R/crates/ritornello-proto/src/lib.rs"
  for c in "${non_companions[@]}"; do [ "$c" = ritornello-plugin-radio ] || rel_bump "$c"; done
  expect_rel 1 "" "ritornello-plugin-radio" "a wire break with one plugin not moved is refused, and names it"

  rel_repo
  printf 'pub const PROTOCOL_VERSION: u32 = 2;\n' > "$R/crates/ritornello-proto/src/lib.rs"
  for c in "${non_companions[@]}"; do rel_bump "$c"; done
  rel_bump ritornello-files-mount
  expect_rel 0 "$all_crates" "" "a wire break with the companion moved too: the companion is printed because it moved"

  rel_repo
  printf '[workspace.package]\nversion = "1.0.0"\n' > "$R/Cargo.toml"
  expect_rel 0 "$all_non_companions" "major moved" "a new major republishes everything, whatever moved"

  # One contract's major moved: the core and the plugins that speak it must
  # move; a plugin that does not speak it is left alone. The expected lists
  # are written out, not computed with speaks(): a speaks() that is wrong
  # would otherwise compute the same wrong list on both sides.
  rel_repo
  contract_set DISPLAY "2, 0"
  for c in ritornello-core ritornello-plugin-console ritornello-plugin-mpd; do rel_bump "$c"; done
  expect_rel 0 "ritornello-core ritornello-plugin-console ritornello-plugin-mpd" "display contract major moved" "a display major with its speakers moved: passes, and radio (source only), not moved, is neither required nor printed"

  rel_repo
  contract_set DISPLAY "2, 0"
  for c in ritornello-core ritornello-plugin-console; do rel_bump "$c"; done
  expect_rel 1 "" "ritornello-plugin-mpd" "a display major with mpd (display + input + admin) not moved is refused, and names it"

  rel_repo
  contract_set DISPLAY "2, 0"
  for c in ritornello-plugin-console ritornello-plugin-mpd; do rel_bump "$c"; done
  expect_rel 1 "" "ritornello-core" "a display major with the core not moved is refused, and names it"

  rel_repo
  contract_set ADMIN "2, 0"
  admin_plugins="ritornello-core ritornello-plugin-cd ritornello-plugin-files ritornello-plugin-generic-input ritornello-plugin-mpd ritornello-plugin-musicbrainz ritornello-plugin-radio"
  for c in $admin_plugins; do rel_bump "$c"; done
  expect_rel 0 "$admin_plugins" "admin contract major moved" "an admin major with the core and the six admin plugins moved: console and the metadata-only plugins are not asked to"

  rel_repo
  contract_set ADMIN "2, 0"
  for c in ritornello-core ritornello-plugin-cd ritornello-plugin-files ritornello-plugin-generic-input ritornello-plugin-mpd ritornello-plugin-radio; do rel_bump "$c"; done
  expect_rel 1 "" "ritornello-plugin-musicbrainz" "an admin major with musicbrainz (metadata + admin) not moved is refused, and names it"

  rel_repo
  contract_set SOURCE "1, 1"
  for c in ritornello-core ritornello-plugin-cd; do rel_bump "$c"; done
  expect_rel 0 "ritornello-core ritornello-plugin-cd" "" "a source MINOR move asks nothing of anyone: only what moved is printed"

  rel_repo
  printf 'pub fn nothing() {}\n' > "$R/crates/ritornello-proto/src/contract.rs"
  expect_rel 1 "" "cannot read the source contract version" "a contract.rs this script cannot read is refused, not skipped"

  # The first release with contracts: PREV has no contract.rs at all. The
  # bootstrap (PROTOCOL_VERSION) already asks everything to move, so no
  # contract rule applies; only the plugin that was bumped is printed.
  rel_repo
  git_rm_baseline_contract
  rel_bump ritornello-plugin-cd
  expect_rel 0 "ritornello-plugin-cd" "" "contract.rs absent at the previous release: no contract rule applies"

  rel_repo
  git_rm_baseline_contract 'pub fn nothing() {}'
  rel_bump ritornello-plugin-cd
  expect_rel 1 "" "cannot read any contract version" "a contract.rs that exists at the previous release but no longer parses is refused, not taken for a first release"

  # Rewrites one section of the repository's fixture: <name> <header> <sample line>.
  wire_set() {
    local f="$R/crates/ritornello-proto/tests/wire-fingerprint.txt"
    tr -d '\r' < "$f" | awk -v n="$1" -v h="$2" -v l="$3" '
      /^\[/ { split(substr($0, 2, length($0) - 2), w, " "); on = (w[1] == n); if (on) { print h; print l; next } }
      !on { print }' > "$f.tmp" && mv "$f.tmp" "$f"
  }

  rel_repo
  rel_bump ritornello-core
  wire_set source "[source 1.0 next]" "Sample = SOURCE"
  expect_rel 1 "" "still marked" "a section still marked next is refused at release: the release is what publishes it"

  rel_repo
  rel_bump ritornello-core
  wire_set source "[source 1.0]" "Sample = CHANGED"
  expect_rel 1 "" "did not move" "a section changed under a version the baseline published is refused" --guard-baseline v0.1.0 v0.1.0

  rel_repo
  rel_bump ritornello-core
  contract_set SOURCE "1, 1"
  wire_set source "[source 1.1]" "Sample = CHANGED"
  expect_rel 0 "ritornello-core" "" "a section changed under a raised version passes" --guard-baseline v0.1.0 v0.1.0

  rel_repo
  rel_bump ritornello-core
  wire_set source "[source 1.0]" "Sample = CHANGED"
  expect_rel 0 "ritornello-core" "" "no published baseline: nothing to compare, only the mark is checked"

  rel_repo
  guard_git rm -q crates/ritornello-proto/tests/wire-fingerprint.txt
  guard_git commit -q -m "baseline without a fixture"; guard_git tag -f v0.1.0 > /dev/null
  guard_git checkout -q HEAD~1 -- crates/ritornello-proto/tests/wire-fingerprint.txt
  if guard_git show v0.1.0:crates/ritornello-proto/tests/wire-fingerprint.txt > /dev/null 2>&1; then
    echo "self-test: the baseline without a fixture still holds one" >&2; fails=$((fails + 1))
  fi
  rel_bump ritornello-core
  wire_set source "[source 1.0]" "Sample = CHANGED"
  expect_rel 0 "ritornello-core" "" "a baseline without a fixture (beta.5) has nothing to compare" --guard-baseline v0.1.0 v0.1.0

  # The shape v0.2.0-beta.6 is measured against: a fixture from before the
  # contracts, one PROTOCOL_VERSION line and no section at all. Its sections
  # are found nowhere, so nothing is compared, even under an unmoved version.
  rel_repo
  printf 'PROTOCOL_VERSION=1\nCommand::Next = {"cmd":"Next"}\n' > "$R/crates/ritornello-proto/tests/wire-fingerprint.txt.old"
  cp "$R/crates/ritornello-proto/tests/wire-fingerprint.txt" "$R/fixture.keep"
  mv "$R/crates/ritornello-proto/tests/wire-fingerprint.txt.old" "$R/crates/ritornello-proto/tests/wire-fingerprint.txt"
  guard_git add -A; guard_git commit -q -m "baseline with a sectionless fixture"; guard_git tag -f v0.1.0 > /dev/null
  mv "$R/fixture.keep" "$R/crates/ritornello-proto/tests/wire-fingerprint.txt"
  rel_bump ritornello-core
  wire_set source "[source 1.0]" "Sample = CHANGED"
  expect_rel 0 "ritornello-core" "" "a baseline whose fixture predates the sections (beta.5's) has no section to compare" --guard-baseline v0.1.0 v0.1.0

  # A version lower than the published one is refused, whatever its lines.
  rel_repo
  contract_set SOURCE "1, 1"
  wire_set source "[source 1.1]" "Sample = SOURCE"
  guard_git add -A; guard_git commit -q -m "baseline publishing source 1.1"; guard_git tag -f v0.1.0 > /dev/null
  contract_set SOURCE "1, 0"
  wire_set source "[source 1.0]" "Sample = SOURCE"
  rel_bump ritornello-core
  expect_rel 1 "" "never goes down" "a section whose minor went below the published one is refused" --guard-baseline v0.1.0 v0.1.0

  # The major decides before the minor: 1.1 is below 2.0 although 1 > 0.
  rel_repo
  contract_set SOURCE "2, 0"
  wire_set source "[source 2.0]" "Sample = SOURCE"
  guard_git add -A; guard_git commit -q -m "baseline publishing source 2.0"; guard_git tag -f v0.1.0 > /dev/null
  contract_set SOURCE "1, 1"
  wire_set source "[source 1.1]" "Sample = SOURCE"
  rel_bump ritornello-core
  expect_rel 1 "" "never goes down" "a section whose major went below the published one is refused" --guard-baseline v0.1.0 v0.1.0

  # A published header this script cannot read is refused, not compared as text.
  rel_repo
  wire_set source "[source one]" "Sample = SOURCE"
  guard_git add -A; guard_git commit -q -m "baseline with an unreadable header"; guard_git tag -f v0.1.0 > /dev/null
  wire_set source "[source 1.0]" "Sample = SOURCE"
  rel_bump ritornello-core
  expect_rel 1 "" "cannot compare" "a published section whose version cannot be read is refused" --guard-baseline v0.1.0 v0.1.0

  # A git that cannot read the baseline's tree is not a baseline without a
  # fixture: the tree holding the fixture is deleted from the object store
  # once the change is committed (a commit would write it back otherwise).
  # Read as "absent", the section changed under its published version would
  # pass.
  rel_repo
  rel_bump ritornello-core
  wire_set source "[source 1.0]" "Sample = CHANGED"
  guard_git add -A; guard_git commit -q -m "core moved, source changed"
  lost=$(guard_git rev-parse v0.1.0:crates/ritornello-proto/tests)
  rm -f "$R/.git/objects/${lost:0:2}/${lost:2}"
  expect_rel 1 "" "git ls-tree" "a baseline tree git cannot read is refused, not taken for one without a fixture" --guard-baseline v0.1.0 v0.1.0

  rel_repo
  rel_bump ritornello-core
  rm "$R/crates/ritornello-proto/tests/wire-fingerprint.txt"
  expect_rel 1 "" "is missing" "a release without the fixture is refused, not taken for one with nothing to check"

  # Layouts a line match would miss but a TOML parser reads: every one of them
  # must still count as speaking, so the unmoved plugin is refused.
  rel_repo
  contract_set INPUT "2, 0"
  for c in ritornello-core ritornello-plugin-generic-input; do rel_bump "$c"; done
  expect_rel 1 "" "ritornello-plugin-mpd" "an input major with mpd (whose SECOND kind is input) not moved is refused"

  rel_repo
  contract_set ADMIN "2, 0"
  printf '[package]
name = "ritornello-plugin-musicbrainz"
version = "0.1.0"

[package.metadata.ritornello]
kinds = ["metadata"]
admin=true # served by a page
' > "$R/crates/ritornello-plugin-musicbrainz/Cargo.toml"
  for c in ritornello-core ritornello-plugin-cd ritornello-plugin-files ritornello-plugin-generic-input ritornello-plugin-mpd ritornello-plugin-radio; do rel_bump "$c"; done
  expect_rel 1 "" "admin contract major moved" "admin=true with a trailing comment still speaks admin, and the refusal is the contract's, not a parse failure"

  rel_repo
  contract_set DISPLAY "2, 0"
  printf '[package]
name = "ritornello-plugin-console"
version = "0.1.0"

[package.metadata.ritornello]
kinds = [
  "display",
]
' > "$R/crates/ritornello-plugin-console/Cargo.toml"
  for c in ritornello-core ritornello-plugin-mpd; do rel_bump "$c"; done
  expect_rel 1 "" "display contract major moved" "a multi-line kinds array still speaks display, and the refusal is the contract's, not a parse failure"

  # A declaration that cannot be read is never "does not speak": the release
  # is refused, naming the crate, whatever the cause (bad UTF-8, a wrong shape).
  rel_repo
  contract_set ADMIN "2, 0"
  printf '[package]\nname = "ritornello-plugin-musicbrainz"\nversion = "0.1.0"\n# \377\376\n' > "$R/crates/ritornello-plugin-musicbrainz/Cargo.toml"
  # The core and every admin speaker moved, the unreadable one included: no
  # other refusal is left, so exit 1 can only be the abort (read as "does not
  # speak", it would pass with 0).
  for c in $admin_plugins; do rel_bump "$c"; done
  expect_rel 1 "" "cannot tell whether ritornello-plugin-musicbrainz speaks" "a manifest that is not valid UTF-8 aborts the release instead of reading as a silent plugin (every speaker is bumped, so only the abort can refuse)"

  rel_repo
  contract_set DISPLAY "2, 0"
  printf '[package]\nname = "ritornello-plugin-console"\nversion = "0.1.0"\n\n[package.metadata.ritornello]\nkinds = 5\n' > "$R/crates/ritornello-plugin-console/Cargo.toml"
  for c in ritornello-core ritornello-plugin-console ritornello-plugin-mpd; do rel_bump "$c"; done
  expect_rel 1 "" "cannot tell whether ritornello-plugin-console speaks" "kinds = 5 aborts the release instead of reading as a silent plugin (every display speaker is bumped, so only the abort can refuse)"

  # The language packs, kept in the output this time: they are data, and a
  # wire break must not republish them under their unchanged numbers.
  rel_repo
  printf 'pub const PROTOCOL_VERSION: u32 = 2;\n' > "$R/crates/ritornello-proto/src/lib.rs"
  for c in "${non_companions[@]}"; do rel_bump "$c"; done
  KEEP_PACKS=1 expect_rel 0 "$all_non_companions" "" "a wire break with the packs untouched: no language pack is printed"

  rel_repo
  printf '[workspace.package]\nversion = "1.0.0"\n' > "$R/Cargo.toml"
  KEEP_PACKS=1 expect_rel 0 "$all_with_packs" "" "a new major republishes every language pack too"

  rel_repo
  printf '[workspace.package]\nversion = "1.0.0"\n' > "$R/Cargo.toml"
  pack_bump "${LANGS[0]}"
  KEEP_PACKS=1 expect_rel 0 "$all_with_packs" "" "a new major with one pack bumped: every pack is printed, that one included"

  rel_repo
  pack_bump "${LANGS[0]}"
  KEEP_PACKS=1 expect_rel 0 "$(pack_archive_name "${LANGS[0]}")" "" "an ordinary release with one pack bumped: only that pack"

  rel_repo
  printf 'pub fn nothing() {}\n' > "$R/crates/ritornello-proto/src/lib.rs"
  expect_rel 1 "" "cannot read PROTOCOL_VERSION" "a lib.rs this script cannot read the protocol number from is refused, not skipped"

  rel_repo
  rel_bump ritornello-core
  expect_rel 0 "ritornello-core" "" "an ordinary release: only what moved"

  rel_repo
  expect_rel 0 "$all_crates" "" "no baseline reference at all: every component, as for a first release" --guard-baseline ""

  # The hand-written CONTRACTS list against the constants contract.rs
  # declares, both ways: the names, and the wire name each one is read under.
  declared=$(tr -d '\r' < crates/ritornello-proto/src/contract.rs \
    | sed -n 's/^pub const \([A-Z][A-Z_]*_CONTRACT\): ContractVersion = .*/\1/p' | sort | tr '\n' ' ')
  listed=$(for e in "${CONTRACTS[@]}"; do printf '%s\n' "${e#*:}"; done | sort | tr '\n' ' ')
  if [ -z "$declared" ] || [ "$declared" != "$listed" ]; then
    echo "self-test: CONTRACTS lists [${listed% }] but contract.rs declares [${declared% }]: a contract missing from the list is never checked for its speakers" >&2
    fails=$((fails + 1))
  fi
  for e in "${CONTRACTS[@]}"; do
    if [ "$(printf '%s' "${e%%:*}" | tr 'a-z' 'A-Z')_CONTRACT" != "${e#*:}" ]; then
      echo "self-test: CONTRACTS entry $e reads its constant under another contract's wire name" >&2
      fails=$((fails + 1))
    fi
  done

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
  echo "self-test: language-pack change detection, the coupled-change guard and the republication rule ok"
  exit 0
fi

# The coupled-change guard (see run_guard above), against its own baseline.
# Checked before anything is printed, so a refused release leaves no half
# list on stdout.
run_guard || exit 1
run_wire_guard || exit 1

# A wire break republishes nothing unless every component that links
# ritornello-proto moved: an archive rebuilt under its old number is fetched
# by no device, and the release would look complete while delivering a core
# that cannot talk to its plugins. A companion links no shared crate, and a
# language pack is data; neither is asked to move.
if [ -n "$PROTO_BREAK" ]; then
  unmoved=()
  for c in "${CRATES[@]}"; do
    is_companion "$c" && continue
    now=$(version_in < "crates/$c/Cargo.toml")
    then_=$(git show "$PREV:crates/$c/Cargo.toml" 2>/dev/null | version_in || echo absent)
    [ "$now" != "$then_" ] || unmoved+=("$c")
  done
  if [ "${#unmoved[@]}" -gt 0 ]; then
    echo "PROTOCOL_VERSION moved since $PREV, but these components did not move their version:" >&2
    printf '  %s\n' "${unmoved[@]}" >&2
    echo "a wire break needs every core and plugin republished under a new number; bump them in crates/<name>/Cargo.toml" >&2
    exit 1
  fi
fi

# A wire contract whose major moved: the core and the plugins that speak it
# must have moved, or the release is refused. Only they: a plugin that does not
# speak the contract keeps working with the new core and stays where it is, and
# its archive is republished only if its own version moved (the loop below),
# which is why ALL is not set here. A minor move is compatible: it says so and
# asks nothing. A PREV without contract.rs is the first release with contracts;
# the PROTOCOL_VERSION move that introduced them already asked everything.
if [ -n "$PREV" ]; then
  # A contract.rs that exists at PREV but that no constant can be read from is
  # not "a first release with contracts": it is a file this script no longer
  # understands, and skipping it would let a contract break through unchecked.
  # A single constant missing at PREV (a contract added since) is still a skip.
  prev_has_contract_file=
  git cat-file -e "$PREV:crates/ritornello-proto/src/contract.rs" 2>/dev/null && prev_has_contract_file=1
  prev_parsed=0
  for entry in "${CONTRACTS[@]}"; do
    cname=${entry%%:*} cconst=${entry#*:}
    now_c=$(contract_version "$cconst" < crates/ritornello-proto/src/contract.rs 2>/dev/null || echo absent)
    if [ "$now_c" = absent ]; then
      echo "cannot read the $cname contract version from crates/ritornello-proto/src/contract.rs" >&2
      exit 1
    fi
    # `|| true`, not `|| echo absent`: under pipefail a failing `git show`
    # would append a second `absent` to the one contract_version prints.
    then_c=$(git show "$PREV:crates/ritornello-proto/src/contract.rs" 2>/dev/null | contract_version "$cconst" || true)
    [ "$then_c" != absent ] || continue
    prev_parsed=$((prev_parsed + 1))
    if [ "${now_c%%.*}" != "${then_c%%.*}" ]; then
      unmoved=()
      for c in "${CRATES[@]}"; do
        is_companion "$c" && continue
        [ "$c" = ritornello-core ] || speaks "$c" "$cname" || continue
        now=$(version_in < "crates/$c/Cargo.toml")
        then_=$(git show "$PREV:crates/$c/Cargo.toml" 2>/dev/null | version_in || echo absent)
        [ "$now" != "$then_" ] || unmoved+=("$c")
      done
      if [ "${#unmoved[@]}" -gt 0 ]; then
        echo "$cname contract major moved ($then_c -> $now_c) since $PREV, but these components that speak it did not move their version:" >&2
        printf '  %s\n' "${unmoved[@]}" >&2
        echo "a contract break needs the core and every plugin that speaks it republished under a new number; bump them in crates/<name>/Cargo.toml" >&2
        exit 1
      fi
      echo "$cname contract major moved ($then_c -> $now_c) since $PREV — its speakers moved" >&2
    elif [ "$now_c" != "$then_c" ]; then
      echo "$cname contract minor moved ($then_c -> $now_c) since $PREV — compatible, nothing is required" >&2
    fi
  done
  if [ -n "$prev_has_contract_file" ] && [ "$prev_parsed" -eq 0 ]; then
    echo "cannot read any contract version from crates/ritornello-proto/src/contract.rs at $PREV" >&2
    exit 1
  fi
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
  # A wire break or a new major republishes every component, except a
  # companion, which moves only when it changes itself.
  if [ -n "$ALL" ] && ! is_companion "$c"; then
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
  if [ -z "$PREV" ] || [ -n "$MAJOR_MOVED" ]; then
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
