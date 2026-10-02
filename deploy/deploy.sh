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
exec target/release/ritornello-install --from-dir "$OUT" "$@"
