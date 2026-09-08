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
