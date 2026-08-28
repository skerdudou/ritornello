#!/usr/bin/env bash
# The same commands as .github/workflows/ci.yml, in the same order: this is
# where the recipe is validated locally before the YAML runs. If one of the
# two changes, the other must follow.
#
# Run from WSL (cargo and npm live there). An optional argument limits the
# run to one stage: web | rust | e2e.
set -euo pipefail
cd "$(dirname "$0")/.."

stage="${1:-all}"

if [ "$stage" = all ] || [ "$stage" = web ]; then
  echo "== web =="
  npm ci
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
fi

if [ "$stage" = all ] || [ "$stage" = e2e ]; then
  echo "== e2e =="
  # Like the e2e job: the debug core must exist, serve.mjs launches it.
  cargo build --workspace
  (cd web/app && npx playwright install chromium && npm run e2e)
fi
