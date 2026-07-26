#!/usr/bin/env bash
set -euo pipefail
# Exemples de TARGET : armv7-unknown-linux-gnueabihf (Raspberry Pi 2, 32 bits),
# aarch64-unknown-linux-gnu (Pi 3/4/5 ou autre carte ARM 64 bits), x86_64-unknown-linux-gnu.
PI="${PI:-pi@raspberrypi.local}"
TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"
OUT="target/$TARGET/release"

cargo install cross --locked 2>/dev/null || true
cross build --release --workspace --target "$TARGET"

ssh "$PI" 'sudo mkdir -p /usr/local/lib/ritornello/plugins /etc/ritornello'
ssh "$PI" 'sudo mkdir -p /etc/ritornello/locales'
scp -r deploy/locales "$PI:/tmp/locales"
ssh "$PI" 'sudo cp -r /tmp/locales/. /etc/ritornello/locales/ && rm -rf /tmp/locales'

ssh "$PI" 'sudo mkdir -p /etc/ritornello/input-presets'
scp -r deploy/input-presets "$PI:/tmp/input-presets"
ssh "$PI" 'sudo cp -r /tmp/input-presets/. /etc/ritornello/input-presets/ && rm -rf /tmp/input-presets'

scp "$OUT/ritornello-core" "$PI:/tmp/ritornello-core"
scp "$OUT/ritornello-plugin-radio" "$OUT/ritornello-plugin-cd" "$OUT/ritornello-plugin-generic-input" "$OUT/ritornello-plugin-console" "$PI:/tmp/"
scp deploy/ritornello.service "$PI:/tmp/"

ssh "$PI" 'sudo mv /tmp/ritornello-core /usr/local/bin/ritornello-core \
  && sudo mv /tmp/ritornello-plugin-radio /tmp/ritornello-plugin-cd /tmp/ritornello-plugin-generic-input /tmp/ritornello-plugin-console /usr/local/lib/ritornello/plugins/ \
  && sudo chmod +x /usr/local/lib/ritornello/plugins/* \
  && sudo rm -f /usr/local/lib/ritornello/plugins/ritornello-plugin-mce \
  && sudo mv /tmp/ritornello.service /etc/systemd/system/ \
  && sudo systemctl daemon-reload \
  && sudo systemctl enable ritornello \
  && sudo systemctl restart ritornello \
  && systemctl status ritornello --no-pager'
echo "OK — logs : ssh $PI journalctl -u ritornello -f"
