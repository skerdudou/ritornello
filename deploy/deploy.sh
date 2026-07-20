#!/usr/bin/env bash
set -euo pipefail
PI="${PI:-pi@raspberrypi.local}"
TARGET=armv7-unknown-linux-gnueabihf
OUT="target/$TARGET/release"

cargo install cross --locked 2>/dev/null || true
cross build --release --workspace --target "$TARGET"

ssh "$PI" 'sudo mkdir -p /usr/local/lib/radio-pi/plugins /etc/radio-pi'

scp "$OUT/radio-pi-core" "$PI:/tmp/radio-pi-core"
scp "$OUT/radio-pi-plugin-radio" "$OUT/radio-pi-plugin-cd" "$OUT/radio-pi-plugin-mce" "$PI:/tmp/"
scp deploy/radio-pi.service "$PI:/tmp/"

ssh "$PI" 'sudo mv /tmp/radio-pi-core /usr/local/bin/radio-pi-core \
  && sudo mv /tmp/radio-pi-plugin-radio /tmp/radio-pi-plugin-cd /tmp/radio-pi-plugin-mce /usr/local/lib/radio-pi/plugins/ \
  && sudo chmod +x /usr/local/lib/radio-pi/plugins/* \
  && sudo mv /tmp/radio-pi.service /etc/systemd/system/ \
  && sudo systemctl daemon-reload \
  && sudo systemctl enable radio-pi \
  && sudo systemctl restart radio-pi \
  && systemctl status radio-pi --no-pager'
echo "OK — logs : ssh $PI journalctl -u radio-pi -f"
