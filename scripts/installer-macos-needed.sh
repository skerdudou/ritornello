#!/usr/bin/env bash
# Decides whether the macOS legs of the workstation installer run: prints
# `run=true` or `run=false` (for $GITHUB_OUTPUT), and why on stderr.
#
# The macOS runners were too often missing (cancelled after 15 minutes with
# not one step run), and each time a pull request about the application,
# Dependabot's included, stayed blocked on a leg that tested nothing. The
# application never runs on macOS; only `ritornello-install` does. So the
# macOS legs run:
#
# - on an installer tag `installer-vX.Y.Z`, whose `publish-installer`
#   attaches their archives;
# - on a pull request or a push that changed `crates/ritornello-install/`.
#
# Nowhere else: not on a product tag, and not when only something the
# installer *uses* changed (its workspace crates, the files it embeds, a
# dependency in Cargo.lock). That is a decision, not an oversight: a macOS
# breakage from those is found, and fixed all at once, the next time the
# installer's crate itself changes. When the change cannot be computed (no
# base, an unknown base), the legs run: a doubt costs a macOS run, never a
# silent skip.
#
# It lives here rather than inline in ci.yml so that `--self-test` can prove
# each branch of the decision on a development machine.
#
# Input, from the environment:
#   REF   — github.ref
#   BASE  — the commit to diff against: the pull request's base, or the
#           push's `before`; empty or all zeros when there is none
#   HEAD_REF — what to diff to (default HEAD; the self-test sets it)
#
# Usage: installer-macos-needed.sh | installer-macos-needed.sh --self-test

set -euo pipefail

WATCHED=crates/ritornello-install/

decide() {
  local ref=${REF:-} base=${BASE:-} head=${HEAD_REF:-HEAD}
  case "$ref" in
    refs/tags/installer-v*)
      echo "an installer tag: its archives are published" >&2
      echo run=true; return ;;
    refs/tags/*)
      echo "a product tag: the application never runs on macOS" >&2
      echo run=false; return ;;
  esac
  # An all-zero `before` (a new ref) is not a commit either, so cat-file
  # covers it.
  if [ -z "$base" ] || ! git cat-file -e "$base^{commit}" 2>/dev/null; then
    echo "no usable base ($base): running, rather than skipping on a doubt" >&2
    echo run=true; return
  fi
  local changed
  changed=$(git diff --name-only "$base" "$head" -- "$WATCHED")
  if [ -n "$changed" ]; then
    echo "the installer's crate changed:" >&2
    echo "$changed" >&2
    echo run=true
  else
    echo "nothing changed under $WATCHED" >&2
    echo run=false
  fi
}

self_test() {
  local here tmp fails=0
  here=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' RETURN
  (
    cd "$tmp"
    git init -q
    git config user.email t@t && git config user.name t
    mkdir -p crates/ritornello-install other
    echo a > crates/ritornello-install/src.rs; echo a > other/app.rs
    git add -A && git commit -qm A
    echo b > other/app.rs; git commit -qam B
    echo c > crates/ritornello-install/src.rs; git commit -qam C
  )
  local A B C
  A=$(git -C "$tmp" rev-parse HEAD~2); B=$(git -C "$tmp" rev-parse HEAD~1); C=$(git -C "$tmp" rev-parse HEAD)
  check() { # name expected REF BASE HEAD_REF
    local got
    got=$(cd "$tmp" && REF=$3 BASE=$4 HEAD_REF=$5 bash "$here" 2>/dev/null)
    if [ "$got" = "run=$2" ]; then echo "ok   $1"; else echo "FAIL $1: got '$got', want run=$2"; fails=$((fails + 1)); fi
  }
  check "installer tag"                 true  refs/tags/installer-v1.0.0 ""    HEAD
  check "finished product tag"          false refs/tags/v1.0.0           ""    HEAD
  check "prerelease product tag"        false refs/tags/v1.0.0-beta.1    ""    HEAD
  check "application change only"       false refs/pull/1/merge          "$A"  "$B"
  check "installer crate changed"       true  refs/pull/1/merge          "$B"  "$C"
  check "push to main, installer"       true  refs/heads/main            "$A"  "$C"
  check "push to main, application"     false refs/heads/main            "$A"  "$B"
  check "no base"                       true  refs/heads/main            ""    HEAD
  check "all-zero base (new ref)"       true  refs/heads/main            0000000000000000000000000000000000000000 HEAD
  check "unknown base"                  true  refs/pull/1/merge          1111111111111111111111111111111111111111 HEAD
  [ "$fails" -eq 0 ] || { echo "$fails case(s) failed" >&2; return 1; }
}

if [ "${1:-}" = --self-test ]; then self_test; else decide; fi
