#!/usr/bin/env bash
# Chaine de build complete de ritornello.
#
# Le build npm ne tourne **qu'une fois** : les livrables qu'il depose sont lus
# par `include_str!`/`rust-embed` a la compilation, donc les deux etapes cargo
# les consomment tels quels. C'est ce qui permet a `cross` de fonctionner avec
# une image Docker sans Node.
set -euo pipefail

TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"

echo "== 1/3 IHM web (npm) =="
npm ci
npm run build --workspaces
npm run typecheck

echo "== 2/3 build natif (x86_64) =="
cargo build --workspace

echo "== 3/3 cross-compilation ($TARGET) =="
cross build --release --workspace --target "$TARGET"

echo "OK"
