#!/usr/bin/env bash
set -euo pipefail
PI="${PI:-pi@raspberrypi.local}"
BIN=target/armv7-unknown-linux-gnueabihf/release/radio-pi

cargo install cross --locked 2>/dev/null || true
cross build --release --target armv7-unknown-linux-gnueabihf

scp "$BIN" "$PI:/tmp/radio-pi"
scp deploy/radio-pi.service "$PI:/tmp/"
ssh "$PI" 'sudo mv /tmp/radio-pi /usr/local/bin/radio-pi \
  && sudo mkdir -p /etc/radio-pi \
  && sudo mv /tmp/radio-pi.service /etc/systemd/system/ \
  && sudo systemctl daemon-reload \
  && sudo systemctl enable radio-pi \
  && sudo systemctl restart radio-pi \
  && systemctl status radio-pi --no-pager'
echo "OK — logs : ssh $PI journalctl -u radio-pi -f"
