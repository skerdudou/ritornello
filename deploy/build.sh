#!/usr/bin/env bash
# Ritornello's complete build chain.
#
# The npm build runs **only once**: the artifacts it drops are read by
# `include_str!`/`rust-embed` at compile time, so both cargo steps consume
# them as-is. This is what lets `cross` work with a Docker image that has
# no Node.
set -euo pipefail

# Always from the repository root, like deploy.sh: launchable from anywhere.
cd "$(dirname "$0")/.."

TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"

echo "== 1/3 web UI (npm) =="
npm ci
npm run build --workspaces
npm run typecheck

echo "== 2/3 native build (x86_64) =="
cargo build --workspace

echo "== 3/3 cross-compilation ($TARGET) =="
cross build --release --workspace --target "$TARGET"

echo "OK"
