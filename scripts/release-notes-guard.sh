#!/usr/bin/env bash
# Refuses to draft a release whose notes still say "Nothing to do" when a
# systemd unit, a polkit rule or the privileged updater changed since the
# previous published release. This is the one place in the whole system that
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
# crates/ritornello-updater` is non-empty (or there is no previous release to
# diff against), .github/release-notes-template.md must already open with
# "**Action required**". A human edits that file before tagging; this only
# checks the edit was made. No auto-fill, no templating: a default that
# silently claimed "action required" without saying what to do by hand would
# be worse than the silence it replaces — a loud stop is the design here, the
# same choice changed-components.sh makes when nothing moved.
#
# Usage: release-notes-guard.sh [ref]
#   with a ref    — compare deploy/*.service, deploy/*.rules and
#                   crates/ritornello-updater against it
#   without a ref — first release: nothing to compare against, so the guard
#                   requires the same opening line unconditionally
#
# What this deliberately does NOT cover: any other privileged file a future
# component might introduce outside these three paths (say, a new file
# staged by deploy/packaging.toml) trips no alarm here. Ruling 52 named
# exactly this set, on purpose — a wider net would also fire on changes that
# need no action by hand and teach everyone to click past the warning. Its
# silence on anything else is a scope, not a guarantee.
set -euo pipefail

cd "$(dirname "$0")/.."
PREV="${1:-}"
TEMPLATE=".github/release-notes-template.md"
WATCHED=(deploy/*.service deploy/*.rules crates/ritornello-updater)

if [ -n "$PREV" ] && ! git rev-parse --verify -q "$PREV^{commit}" >/dev/null; then
  echo "$PREV is not a commit — pass a release tag, or no argument for a first release" >&2
  exit 1
fi

if [ -n "$PREV" ] && git diff --quiet "$PREV" -- "${WATCHED[@]}"; then
  echo "no unit, rule or updater change since $PREV — no guard to enforce"
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
  echo "a systemd unit, a polkit rule or the updater changed since ${PREV:-the start (first release)}, but $TEMPLATE still opens with:"
  echo "  $opening"
  echo "The updater cannot write any of those, by design (see"
  echo "docs/installation.md#enabling-automatic-updates-once-by-hand)."
  echo "Edit the first line of $TEMPLATE to"
  echo "'**Action required** — <what to do by hand>', commit it, then tag."
} >&2
exit 1
