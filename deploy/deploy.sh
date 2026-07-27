#!/usr/bin/env bash
set -euo pipefail
# Exemples de TARGET : armv7-unknown-linux-gnueabihf (Raspberry Pi 2, 32 bits),
# aarch64-unknown-linux-gnu (Pi 3/4/5 ou autre carte ARM 64 bits), x86_64-unknown-linux-gnu.
PI="${PI:-pi@raspberrypi.local}"
TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"
OUT="target/$TARGET/release"

# Toujours depuis la racine du depot : tous les chemins ci-dessous en dependent,
# et le script doit pouvoir etre lance de n'importe ou.
cd "$(dirname "$0")/.."

# La liste des plugins n'existe qu'ici : elle sert au scp puis au mv distant,
# et une liste dupliquee entre les deux finirait par diverger.
PLUGINS=(radio cd generic-input console musicbrainz ouifm-metas)

if ! command -v cross >/dev/null; then
  # Pas de `2>/dev/null || true` : si l'installation echoue, son diagnostic
  # est la seule explication du « command not found » qui suivrait.
  cargo install cross --locked
fi

# Le build complet, npm compris : `cross build` seul embarquerait le
# `web/app/dist` present sur le disque — un bouchon sur un clone frais
# (« Web interface not built » livre sur l'appareil), ou pire une IHM perimee,
# sans aucun avertissement. build.sh fait les etapes dans le bon ordre.
./deploy/build.sh

ssh "$PI" 'sudo mkdir -p /usr/local/lib/ritornello/plugins /etc/ritornello'

# `rm -rf` prealable des zones de transit : si un deploiement precedent a
# echoue entre le scp et l'installation, `scp -r` vers un repertoire residuel
# creerait /tmp/locales/locales, et un sous-repertoire parasite partirait
# dans /etc/ritornello/locales.
ssh "$PI" 'sudo mkdir -p /etc/ritornello/locales && rm -rf /tmp/locales'
scp -r deploy/locales "$PI:/tmp/locales"
ssh "$PI" 'sudo cp -r /tmp/locales/. /etc/ritornello/locales/ && rm -rf /tmp/locales'

ssh "$PI" 'sudo mkdir -p /etc/ritornello/input-presets && rm -rf /tmp/input-presets'
scp -r deploy/input-presets "$PI:/tmp/input-presets"
ssh "$PI" 'sudo cp -r /tmp/input-presets/. /etc/ritornello/input-presets/ && rm -rf /tmp/input-presets'

scp "$OUT/ritornello-core" "$PI:/tmp/ritornello-core"
scp "${PLUGINS[@]/#/$OUT/ritornello-plugin-}" "$PI:/tmp/"
scp deploy/ritornello.service "$PI:/tmp/"

DEPLACE_PLUGINS=$(printf '/tmp/ritornello-plugin-%s ' "${PLUGINS[@]}")
ssh "$PI" "sudo mv /tmp/ritornello-core /usr/local/bin/ritornello-core \
  && sudo mv $DEPLACE_PLUGINS /usr/local/lib/ritornello/plugins/ \
  && sudo chmod +x /usr/local/lib/ritornello/plugins/* \
  && sudo rm -f /usr/local/lib/ritornello/plugins/ritornello-plugin-mce \
  && sudo mv /tmp/ritornello.service /etc/systemd/system/ \
  && sudo systemctl daemon-reload \
  && sudo systemctl enable ritornello \
  && sudo systemctl restart ritornello \
  && systemctl status ritornello --no-pager"
echo "OK — logs : ssh $PI journalctl -u ritornello -f"
