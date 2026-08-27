#!/usr/bin/env bash
# Les mêmes commandes que .github/workflows/ci.yml, dans le même ordre : le
# dépôt n'a pas encore de remote, donc c'est ici que la recette est validée
# avant que le YAML ne tourne pour la première fois. Si l'un des deux change,
# l'autre doit suivre.
#
# À lancer depuis WSL (cargo et npm y vivent). Un argument optionnel limite
# l'exécution à une étape : web | rust | e2e.
set -euo pipefail
cd "$(dirname "$0")/.."

etape="${1:-tout}"

if [ "$etape" = tout ] || [ "$etape" = web ]; then
  echo "== web =="
  npm ci
  npm run build --workspaces --if-present
  npm run typecheck
  npm test --workspaces --if-present
fi

if [ "$etape" = tout ] || [ "$etape" = rust ]; then
  echo "== rust =="
  # Le même refus qu'en CI : sans dist, cargo embarque un bouchon en silence.
  test -f web/app/dist/index.html && ls crates/*/ui/dist/ui.js >/dev/null
  cargo build --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
fi

if [ "$etape" = tout ] || [ "$etape" = e2e ]; then
  echo "== e2e =="
  # Comme le job e2e : le cœur en debug doit exister, serve.mjs le lance.
  cargo build --workspace
  (cd web/app && npx playwright install chromium && npm run e2e)
fi
