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
# Usage: changed-components.sh [ref]
#   with a ref    — the components whose declared version differs from it
#   without a ref — every component (the first release, or an unknown history)
set -euo pipefail

cd "$(dirname "$0")/.."
PREV="${1:-}"

if [ -n "$PREV" ] && ! git rev-parse --verify -q "$PREV^{commit}" >/dev/null; then
  echo "$PREV is not a commit — pass a release tag, or no argument for a first release" >&2
  exit 1
fi

# Shared crates are linked into every component's binary and inherit the
# product version, so changing one moves no declared number while rebuilding
# all eleven. Publishing "what changed" would then leave ten plugins on the
# device built against the old crate, with version equality claiming
# everything is up to date. A change to any of them therefore counts as a
# change to everything — decided by content, not by a number, and failing in
# the safe direction.
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

if [ "$changed" -eq 0 ]; then
  echo "no component version moved since $PREV — bump the component you fixed, or this release delivers nothing" >&2
  exit 2
fi
