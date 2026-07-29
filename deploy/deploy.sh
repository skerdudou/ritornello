#!/usr/bin/env bash
set -euo pipefail
# TARGET examples: armv7-unknown-linux-gnueabihf (Raspberry Pi 2, 32-bit),
# aarch64-unknown-linux-gnu (Pi 3/4/5 or other 64-bit ARM board), x86_64-unknown-linux-gnu.
PI="${PI:-pi@raspberrypi.local}"
TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"
OUT="target/$TARGET/release"

# Always from the repository root: every path below depends on it, and the
# script must be launchable from anywhere.
cd "$(dirname "$0")/.."

# The plugin list exists only here: it drives the scp then the remote mv,
# and a list duplicated between the two would end up diverging.
PLUGINS=(radio cd generic-input console musicbrainz ouifm-metas)

if ! command -v cross >/dev/null; then
  # No `2>/dev/null || true`: if the installation fails, its diagnostic is
  # the only explanation for the "command not found" that would follow.
  cargo install cross --locked
fi

# The full build, npm included: `cross build` alone would embed whatever
# `web/app/dist` sits on disk — a placeholder on a fresh clone ("Web
# interface not built" shipped to the device), or worse a stale UI, with
# no warning at all. build.sh runs the steps in the right order.
./deploy/build.sh

ssh "$PI" 'sudo mkdir -p /usr/local/lib/ritornello/plugins /etc/ritornello'

# The service runs unprivileged: a dedicated system user, created on first
# deployment (device access comes through the groups declared in the unit,
# not through useradd -G). Its home is the state directory, the only place
# a subprocess (mpv) could want to write to.
ssh "$PI" 'id -u ritornello >/dev/null 2>&1 \
  || sudo useradd --system --home-dir /var/lib/ritornello --no-create-home \
       --shell /usr/sbin/nologin ritornello'

# Prior `rm -rf` of the staging areas: if a previous deployment failed
# between the scp and the installation, `scp -r` into a leftover directory
# would create /tmp/locales/locales, and a stray subdirectory would end up
# in /etc/ritornello/locales.
ssh "$PI" 'sudo mkdir -p /etc/ritornello/locales && rm -rf /tmp/locales'
scp -r deploy/locales "$PI:/tmp/locales"
ssh "$PI" 'sudo cp -r /tmp/locales/. /etc/ritornello/locales/ && rm -rf /tmp/locales'

ssh "$PI" 'sudo mkdir -p /etc/ritornello/input-presets && rm -rf /tmp/input-presets'
scp -r deploy/input-presets "$PI:/tmp/input-presets"
ssh "$PI" 'sudo cp -r /tmp/input-presets/. /etc/ritornello/input-presets/ && rm -rf /tmp/input-presets'

scp "$OUT/ritornello-core" "$PI:/tmp/ritornello-core"
scp "${PLUGINS[@]/#/$OUT/ritornello-plugin-}" "$PI:/tmp/"
scp deploy/ritornello.service "$PI:/tmp/"

# After every copy into /etc/ritornello, which would hand them back to
# root: the directory belongs to the service, because the radio and
# generic-input plugins persist stations.toml and input-bindings.toml
# there through atomic writes (.tmp then rename), which requires write
# access to the directory itself. /var/lib/ritornello is taken over too:
# a previous root installation left state files there that the service
# could no longer rewrite.
ssh "$PI" 'sudo chown -R ritornello: /etc/ritornello \
  && if [ -d /var/lib/ritornello ]; then sudo chown -R ritornello: /var/lib/ritornello; fi'

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
echo "OK — logs: ssh $PI journalctl -u ritornello -f"
