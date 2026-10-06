#!/usr/bin/env bash
# The same commands as .github/workflows/ci.yml, in the same order: this is
# where the recipe is validated locally before the YAML runs. If one of the
# two changes, the other must follow.
#
# Run from WSL (cargo and npm live there). An optional argument limits the
# run to one stage: web | rust | installer | e2e.
set -euo pipefail
cd "$(dirname "$0")/.."

stage="${1:-all}"

if [ "$stage" = all ] || [ "$stage" = web ]; then
  echo "== web =="
  # CI runs the build in `web-build` and the last two commands in `web-test`,
  # in parallel with the Rust jobs; here they stay in one sequence.
  npm ci
  # Before anything consumes the tree: npm ci proves the lock installs, not
  # that what it describes can be loaded or still covers every platform.
  node scripts/check-lockfile.mjs
  npm run build --workspaces --if-present
  npm run typecheck
  npm test --workspaces --if-present
fi

if [ "$stage" = all ] || [ "$stage" = rust ]; then
  echo "== rust =="
  # The same refusal as in CI: without dist, cargo silently embeds a stub.
  test -f web/app/dist/index.html && ls crates/*/ui/dist/ui.js >/dev/null
  cargo build --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ./scripts/release-notes-guard.sh --self-test
fi

if [ "$stage" = all ] || [ "$stage" = installer ]; then
  echo "== installer =="
  # The `installer` job's Linux leg, the only one a Linux machine can run:
  # its Windows and macOS legs exist in CI alone. Needs musl-gcc
  # (`apt install musl-tools`), which `ring` compiles its C with.
  t=x86_64-unknown-linux-musl
  rustup target add "$t"
  cargo clippy -p ritornello-install --target "$t" --all-targets -- -D warnings
  cargo test -p ritornello-install --target "$t"
  cargo build --release -p ritornello-install --target "$t"
fi

if [ "$stage" = all ] || [ "$stage" = e2e ]; then
  echo "== e2e =="
  # Like the e2e job: the debug core must exist, serve.mjs launches it.
  cargo build --workspace
  (cd web/app && npx playwright install chromium && npm run e2e)
fi
