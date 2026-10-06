#!/usr/bin/env bash
# Refuses to draft a release whose notes still say "Nothing to do" when a
# systemd unit, a polkit rule, the privileged updater or the files mount
# helper (crates/ritornello-files-mount) changed since the previous published
# release. This is the one place in the whole system that
# *knows* such a file moved, and until this guard existed it said nothing
# about it: a release that changed only a unit could publish no archive at
# all (none of those paths moves a component's declared version) and still
# announce "Nothing to do" — the change would then reach no device.
#
# It lives here rather than inline in the workflow YAML for the same reason
# changed-components.sh and package-release.sh do: it must be runnable on a
# development machine and its output looked at, instead of being discovered
# by pushing a tag. The release workflow has never run in this repository.
#
# The rule: when `git diff <ref> -- deploy/*.service deploy/*.rules
# crates/ritornello-updater crates/ritornello-files-mount` is non-empty (or there is no previous release to
# diff against), .github/release-notes-template.md must already open with
# "**Action required**". A human edits that file before tagging; this only
# checks the edit was made. No auto-fill, no templating: a default that
# silently claimed "action required" without saying what to do by hand would
# be worse than the silence it replaces — a loud stop is the design here, the
# same choice changed-components.sh makes when nothing moved.
#
# **Why the mount helper is watched.** It is the companion `files-mount`: a
# root-run binary that only `ritornello-install` places, never the web UI.
# A release that changes it moves the companion's version, and the web UI
# then refuses to update `files` until the operator has run
# `ritornello-install` — which is exactly an action by hand the notes must
# name. Its crate is watched whole, library included: the helper binary is
# built from it.
#
# Usage: release-notes-guard.sh [ref]
#   with a ref    — compare deploy/*.service, deploy/*.rules,
#                   crates/ritornello-updater and crates/ritornello-files-mount
#                   against it
#   without a ref — first release: nothing to compare against, so the guard
#                   requires the same opening line unconditionally
#   --self-test   — runs the guard itself inside throwaway git repositories,
#                   one per branch of its decision, and checks each verdict
#
# What this deliberately does NOT cover: any other privileged file a future
# component might introduce outside these four paths (say, a new file
# staged by deploy/packaging.toml) trips no alarm here. Ruling 52 named
# exactly this set, on purpose — a wider net would also fire on changes that
# need no action by hand and teach everyone to click past the warning. Its
# silence on anything else is a scope, not a guarantee.
set -euo pipefail

if [ "${1:-}" = "--self-test" ]; then
  # Each case: a baseline commit, one change on top of it, the notes' first
  # line, and the exit code the guard must give against the baseline. Run
  # for real, on a copy of this very script, so a WATCHED entry dropped or
  # mistyped reddens the case that needs it.
  self="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
  failures=0
  check() { # <name> <changed path> <notes first line> <expected exit> [version-line|dependency-line]
    local name=$1 path=$2 notes=$3 want=$4 how=${5:-rewrite} dir got
    dir=$(mktemp -d)
    (
      cd "$dir"
      git init -q
      # Not the machine's own setting: on Windows it floods every case with
      # line-ending warnings that say nothing about the guard.
      git config core.autocrlf false
      git config user.email self-test@example.invalid
      git config user.name self-test
      mkdir -p scripts .github deploy crates/ritornello-updater crates/ritornello-files-mount/src crates/other
      cp "$self" scripts/release-notes-guard.sh
      echo 'Nothing to do' > .github/release-notes-template.md
      for f in deploy/a.service deploy/a.rules crates/ritornello-updater/x.rs \
        crates/ritornello-files-mount/src/x.rs crates/other/x.rs; do
        echo base > "$f"
      done
      printf '[package]\nname = "ritornello-files-mount"\nversion = "1.0.0"\n\n[dependencies]\nfoo = "1"\n' \
        > crates/ritornello-files-mount/Cargo.toml
      git add -A && git commit -qm base && git tag base
      case "$how" in
        version-line) sed -i 's/^version = "1.0.0"/version = "2.0.0"/' "$path" ;;
        dependency-line) sed -i 's/^foo = "1"/foo = "2"/' "$path" ;;
        *) echo changed > "$path" ;;
      esac
      echo "$notes" > .github/release-notes-template.md
      git add -A && git commit -qm change
    )
    set +e
    (cd "$dir" && ./scripts/release-notes-guard.sh base >/dev/null 2>&1)
    got=$?
    set -e
    rm -rf "$dir"
    if [ "$got" = "$want" ]; then
      echo "ok   $name (exit $got)"
    else
      echo "FAIL $name: exit $got, expected $want" >&2
      failures=$((failures + 1))
    fi
  }
  check "a unit changed, notes say nothing" deploy/a.service 'Nothing to do' 1
  check "a rule changed, notes say nothing" deploy/a.rules 'Nothing to do' 1
  check "the updater changed, notes say nothing" crates/ritornello-updater/x.rs 'Nothing to do' 1
  check "the mount helper changed, notes say nothing" crates/ritornello-files-mount/src/x.rs 'Nothing to do' 1
  check "the mount helper changed, notes say what to do" crates/ritornello-files-mount/src/x.rs '**Action required** — run ritornello-install' 0
  # The companion's number is a counter that moves only when the companion
  # changes; a diff that is ONLY that line is the bookkeeping of a change
  # already judged elsewhere, not a change to what the installer places.
  check "files-mount version line only: not a mount helper change" crates/ritornello-files-mount/Cargo.toml 'Nothing to do' 0 version-line
  check "files-mount manifest dependency changed" crates/ritornello-files-mount/Cargo.toml 'Nothing to do' 1 dependency-line
  check "nothing watched changed" crates/other/x.rs 'Nothing to do' 0
  [ "$failures" = 0 ] || exit 1
  echo "release-notes-guard.sh --self-test: all cases pass"
  exit 0
fi

cd "$(dirname "$0")/.."
PREV="${1:-}"
TEMPLATE=".github/release-notes-template.md"
WATCHED=(deploy/*.service deploy/*.rules crates/ritornello-updater crates/ritornello-files-mount)

if [ -n "$PREV" ] && ! git rev-parse --verify -q "$PREV^{commit}" >/dev/null; then
  echo "$PREV is not a commit — pass a release tag, or no argument for a first release" >&2
  exit 1
fi

# A manifest with its `[package]` `version =` line set aside: the same idea
# as changed-components.sh's manifest_minus_version.
manifest_minus_version() { # <manifest text on stdin>
  tr -d '\r' | awk '/^\[/ { section = $0 } !(section == "[package]" && /^version = /)'
}

# Whether anything watched changed, not counting a bump of the mount helper's
# own version. That number is a counter moved by one at each real change of
# the helper; the change itself is judged by every other line of the diff, so
# a version line alone says nothing about what the installer must place.
watched_changed() { # <ref>
  local ref="$1" f
  git diff --quiet "$ref" -- "${WATCHED[@]}" && return 1
  while IFS= read -r f; do
    if [ "$f" = crates/ritornello-files-mount/Cargo.toml ] && [ -f "$f" ] \
      && base=$(git show "$ref:$f" 2>/dev/null) \
      && [ "$(printf '%s\n' "$base" | manifest_minus_version)" = "$(manifest_minus_version < "$f")" ]; then
      continue
    fi
    return 0
  done < <(git diff --name-only "$ref" -- "${WATCHED[@]}")
  return 1
}

if [ -n "$PREV" ] && ! watched_changed "$PREV"; then
  echo "no unit, rule, updater or mount helper change since $PREV — no guard to enforce"
  exit 0
fi

opening=$(head -n1 "$TEMPLATE" | tr -d '\r')
case "$opening" in
  '**Action required**'*)
    echo "$TEMPLATE already opens with Action required — guard satisfied"
    exit 0
    ;;
esac

{
  echo "a systemd unit, a polkit rule, the updater or the mount helper changed since ${PREV:-the start (first release)}, but $TEMPLATE still opens with:"
  echo "  $opening"
  echo "The updater cannot write any of those, by design (see"
  echo "docs/installation.md#enabling-automatic-updates-once-by-hand)."
  echo "Edit the first line of $TEMPLATE to"
  echo "'**Action required** — <what to do by hand>', commit it, then tag."
} >&2
exit 1
