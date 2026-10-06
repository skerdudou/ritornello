#!/usr/bin/env bash
# Development wrapper: build everything, package it exactly as a release
# would, then install it with the same program a user runs against a
# published release. One installation path, whether fed from GitHub or from
# this checkout.
#
#   RITORNELLO_HOST=dietpi@192.168.0.57 ./deploy/deploy.sh [ritornello-install options]
#
# TARGET names the device's architecture (see docs/installation.md).
# DEPLOY_STOP_BEFORE_INSTALL=1 stops once the directory handed to
# ritornello-install is complete and the installer is built, without running
# it: what a developer uses to inspect the directory, and what proves the
# chain without a device.
#
# Not a second installer: nothing here places a file on a device. Every
# privileged path, unit and rule comes from inventory.json, which is
# generated from deploy/packaging.toml like the archives are.
set -euo pipefail

# A development deploy ships a local build under the version numbers the
# checkout already declares, and ritornello-install leaves alone whatever the
# device has at those numbers: without --reinstall, changed code under an
# unchanged number would never reach the device. So --reinstall is added,
# unless the arguments already say it, or say --remove-all (which places
# nothing, and which the installer refuses beside --reinstall).
needs_reinstall() {
  local a
  for a in "$@"; do
    case "$a" in --reinstall|--remove-all) return 1 ;; esac
  done
  return 0
}

# `deploy.sh --self-test`: the argument rule above, on its own cases, with
# nothing built and no device. Run by the Rust suite (ritornello-install's
# `deploy_sh_adds_reinstall_unless_told_otherwise`).
if [ "${1:-}" = --self-test ]; then
  fail=0
  check() { # <expected: add|keep> <args...>
    local want=$1; shift
    if needs_reinstall "$@"; then got=add; else got=keep; fi
    if [ "$got" != "$want" ]; then echo "FAIL: [$*] -> $got, want $want" >&2; fail=1; fi
  }
  check add
  check add --keep
  check add --plugins radio,cd --version v0.2.0
  check add --keep --yes --purge-data
  check keep --reinstall
  check keep --keep --reinstall
  check keep --remove-all
  check keep --remove-all --purge-data --yes
  check add --plugins reinstall
  check add --packs --remove-allx
  [ "$fail" = 0 ] && echo "deploy.sh: self-test passed"
  exit "$fail"
fi

# Always from the repository root: every path below depends on it, and the
# script must be launchable from anywhere.
cd "$(dirname "$0")/.."

# TARGET examples: armv7-unknown-linux-gnueabihf (Raspberry Pi 2, 32-bit),
# aarch64-unknown-linux-gnu (Pi 3/4/5 or other 64-bit ARM board),
# x86_64-unknown-linux-gnu.
TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"
case "$TARGET" in
  armv7-*) ARCH=armv7 ;;
  aarch64-*) ARCH=arm64 ;;
  x86_64-*) ARCH=x86_64 ;;
  *) echo "deploy.sh: unknown TARGET $TARGET" >&2; exit 1 ;;
esac

if ! command -v cross >/dev/null; then
  # No `2>/dev/null || true`: if the installation fails, its diagnostic is
  # the only explanation for the "command not found" that would follow.
  cargo install cross --locked
fi

# The full build, npm included: `cross build` alone would embed whatever
# `web/app/dist` sits on disk. build.sh runs the steps in the right order.
./deploy/build.sh

# The same two calls the release job makes, so what is installed here is
# what a release would install.
./scripts/package-release.sh "$TARGET" "$ARCH"
./scripts/package-release.sh --languages

# One directory. SHA256SUMS covers every file in it.
OUT=release/install
rm -rf "$OUT"
mkdir -p "$OUT"
cp release/"$ARCH"/*.tar.gz release/languages/*.tar.gz "$OUT"/
python3 scripts/install-inventory.py > "$OUT/inventory.json"
# Every archive the inventory names must be here: this directory holds every
# component, so a name nothing was built under is caught before the device is
# reached rather than by ritornello-install halfway through its fetches.
python3 - "$OUT" "$ARCH" <<'EOF'
import json, pathlib, sys
out, arch = pathlib.Path(sys.argv[1]), sys.argv[2]
inv = json.loads((out / "inventory.json").read_text(encoding="utf-8"))
names = [inv["core"]["archive"]] + [c["archive"] for c in inv["plugins"] + inv["companions"]]
names = [n.replace("{arch}", arch) for n in names] + [p["archive"] for p in inv["packs"]]
missing = [n for n in names if not (out / n).is_file()]
if missing:
    sys.exit("deploy.sh: inventory.json names archives that were not built: " + ", ".join(missing))
EOF
( cd "$OUT" && sha256sum *.tar.gz inventory.json > SHA256SUMS )

# The installer runs on THIS machine, not on the device: a host build, never
# the cross target. (build.sh already cross-builds it for ARM with the rest
# of the workspace; that copy is useless here and the separate host build is
# on purpose.)
cargo build --release -p ritornello-install

if [ -n "${DEPLOY_STOP_BEFORE_INSTALL:-}" ]; then
  echo "deploy.sh: $OUT is ready; not installing (DEPLOY_STOP_BEFORE_INSTALL)"
  exit 0
fi
if needs_reinstall "$@"; then
  set -- "$@" --reinstall
fi
exec target/release/ritornello-install --from-dir "$OUT" "$@"
