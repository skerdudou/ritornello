#!/usr/bin/env bash
# The two decisions of the release workflow that depend on a tag's NAME, kept
# here so they can be run and broken on a development machine rather than
# discovered by pushing a tag (a workflow change is never testable from its
# own branch).
#
# The installer has its own publication channel: releases tagged
# `installer-vX.Y.Z`, and one fixed release tagged `installer`. They sit in
# the same list as the product's releases, so everything that asks "what is
# the previous release" must skip them, and the tag that publishes one must
# agree with the installer's own version number.
#
# Usage:
#   release-tags.sh newest-product
#       Reads tag names on stdin, newest first (as `gh release list` writes
#       them), and prints the first that is not the installer's. Prints
#       nothing when there is none (nothing was ever published): a first
#       release publishes everything.
#   release-tags.sh check-installer-tag <tag> <Cargo.toml>
#       Exits 0 when <tag> is exactly `installer-v` + the version the
#       manifest declares, and that version is a finished X.Y.Z. A release
#       that is not a prerelease cannot carry a suffix, and a tag that
#       disagrees with the manifest would publish archives named after a
#       number nothing carries.
#   release-tags.sh --self-test
set -euo pipefail

# The two shapes this repository publishes. Not a bare prefix test: `v*` is
# the product's namespace and nothing else may claim it by accident.
is_installer_tag() {
  case "$1" in
    installer | installer-v*) return 0 ;;
    *) return 1 ;;
  esac
}

newest_product() {
  # Reads to the end of its input even after the answer is known: under
  # `set -o pipefail` (the Actions default shell) a reader that stops early
  # can make the writer die on SIGPIPE and fail the whole step.
  local tag found=
  while IFS= read -r tag || [ -n "$tag" ]; do
    tag=${tag%$'\r'}
    [ -n "$tag" ] || continue
    if [ -z "$found" ] && ! is_installer_tag "$tag"; then
      found=$tag
    fi
  done
  [ -z "$found" ] || printf '%s\n' "$found"
}

check_installer_tag() {
  local tag=$1 manifest=$2 version
  # Inside [package] only: a later table such as `[dependencies.x]` may carry
  # a `version = "..."` line of its own, which is not the installer's.
  # A trailing CR is dropped first: a Windows checkout (core.autocrlf) hands
  # WSL `[package]\r`, which never equals `[package]`, and the version then
  # read as empty — the Rust suite failed locally while CI (LF) was green.
  version=$(awk '{ sub(/\r$/, "") } /^\[/ { in_package = ($0 == "[package]") } in_package && /^version = "/ { sub(/^version = "/, ""); sub(/"$/, ""); print; exit }' "$manifest")
  if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "the installer declares version '$version' in $manifest: it must be a finished X.Y.Z (its releases are never prereleases, and it cannot inherit the product's number)" >&2
    return 1
  fi
  if [ "installer-v$version" != "$tag" ]; then
    echo "tag $tag != installer-v$version (the number $manifest declares)" >&2
    return 1
  fi
}

self_test() {
  local failures=0 dir got
  expect() { # <name> <want> <got>
    if [ "$2" != "$3" ]; then
      echo "FAIL: $1 (wanted '$2', got '$3')" >&2
      failures=$((failures + 1))
    fi
  }

  # The baseline queries. Each case would pick the wrong tag if the filter
  # were missing, or too wide, or applied to one shape only.
  expect "skips the fixed installer release at the head" \
    "v0.2.0" "$(printf 'installer\nv0.2.0\nv0.1.0\n' | newest_product)"
  expect "skips a numbered installer release at the head" \
    "v0.2.0" "$(printf 'installer-v0.2.1\nv0.2.0\n' | newest_product)"
  expect "skips both, in either order" \
    "v0.2.0" "$(printf 'installer-v0.2.1\ninstaller\nv0.2.0\n' | newest_product)"
  expect "skips installer releases in the middle too" \
    "v0.3.0-beta.1" "$(printf 'v0.3.0-beta.1\ninstaller\nv0.2.0\n' | newest_product)"
  expect "keeps a product tag whatever it is" \
    "v0.2.0" "$(printf 'v0.2.0\ninstaller\n' | newest_product)"
  expect "nothing published but installers: empty" \
    "" "$(printf 'installer\ninstaller-v0.2.0\n' | newest_product)"
  expect "no input at all: empty" "" "$(printf '' | newest_product)"
  expect "a name that only starts like the installer's is a product tag" \
    "installers-notes" "$(printf 'installers-notes\n' | newest_product)"
  expect "a CRLF line is read like any other" \
    "v0.2.0" "$(printf 'installer\r\nv0.2.0\r\n' | newest_product)"

  # The tag check, against real manifests.
  dir=$(mktemp -d)
  trap 'rm -rf "$dir"' RETURN
  check() { # <name> <want exit> <tag> <version line>
    printf '[package]\nname = "ritornello-install"\n%s\nedition.workspace = true\n\n[dependencies]\nserde = { version = "1" }\n\n[dependencies.other]\nversion = "9.9.9"\n' "$4" > "$dir/Cargo.toml"
    if check_installer_tag "$3" "$dir/Cargo.toml" 2>/dev/null; then got=0; else got=1; fi
    expect "$1" "$2" "$got"
  }
  check "the tag names the declared number" 0 installer-v0.2.0 'version = "0.2.0"'
  check "a higher tag than the manifest is refused" 1 installer-v0.2.1 'version = "0.2.0"'
  check "a lower tag than the manifest is refused" 1 installer-v0.1.9 'version = "0.2.0"'
  check "a product tag is not an installer tag" 1 v0.2.0 'version = "0.2.0"'
  check "the bare tag of the fixed release is not a numbered one" 1 installer 'version = "0.2.0"'
  check "a prerelease number is refused even when the tag agrees" 1 installer-v0.2.0-beta.1 'version = "0.2.0-beta.1"'
  check "an inherited number is refused" 1 installer-v0.2.0 'version.workspace = true'
  check "a two-part number is refused" 1 installer-v0.2 'version = "0.2"'
  # A dependency's own `version` must never be taken for the installer's.
  check "only the package's own line counts" 1 installer-v9.9.9 'version.workspace = true'
  # A Windows checkout writes the manifest with CRLF; it must read the same.
  printf '[package]\r\nname = "ritornello-install"\r\nversion = "0.2.0"\r\n' > "$dir/Cargo.toml"
  if check_installer_tag installer-v0.2.0 "$dir/Cargo.toml" 2>/dev/null; then got=0; else got=1; fi
  expect "a CRLF manifest is read like an LF one" 0 "$got"

  if [ "$failures" -ne 0 ]; then
    echo "release-tags.sh --self-test: $failures failure(s)" >&2
    return 1
  fi
  echo "release-tags.sh --self-test: ok"
}

case "${1:-}" in
  newest-product) newest_product ;;
  check-installer-tag)
    [ "$#" -eq 3 ] || { echo "usage: release-tags.sh check-installer-tag <tag> <Cargo.toml>" >&2; exit 2; }
    check_installer_tag "$2" "$3"
    ;;
  --self-test) self_test ;;
  *)
    echo "usage: release-tags.sh newest-product | check-installer-tag <tag> <Cargo.toml> | --self-test" >&2
    exit 2
    ;;
esac
