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

# The plugin list drives the scp then the remote mv, from this single place:
# a list duplicated between the two would end up diverging.
PLUGINS=(radio cd files generic-input console musicbrainz ouifm-metas radiofrance-metas)

# deploy/plugins.example.toml names the same set, from the core's side — and
# its entries are now installed one by one on a device already in service
# (see further down). The two lists must therefore hold the same names: one
# declared there without its binary here gives the core an exec that does not
# exist, and one built here but absent there ships a plugin nothing launches.
# Both are the mistake of a plugin added in a hurry, and both are silent, so
# they are turned into a refusal to deploy.
DECLARES=$(sed -n 's|^exec *= *".*/ritornello-plugin-\([^"]*\)".*|\1|p' \
  deploy/plugins.example.toml | sort)
if [ "$DECLARES" != "$(printf '%s\n' "${PLUGINS[@]}" | sort)" ]; then
  echo "deploy.sh: PLUGINS and deploy/plugins.example.toml disagree" >&2
  diff <(printf '%s\n' "${PLUGINS[@]}" | sort) <(echo "$DECLARES") >&2 || true
  exit 1
fi

# One password prompt for the whole run: every ssh/scp call below shares a
# single master connection (ControlMaster), opened by the first call and
# closed by the trap. Without an SSH key, the password is asked once
# instead of once per call; with a key or an agent, it is simply faster.
# %C is a hash of user/host/port — short (Unix sockets cap path length)
# and stable across the calls of one run.
SSHOPTS=(-o ControlMaster=auto -o 'ControlPath=/tmp/ritornello-deploy-%C' -o ControlPersist=yes)
fermer_liaison() { ssh "${SSHOPTS[@]}" -O exit "$PI" 2>/dev/null || true; }
trap fermer_liaison EXIT

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

ssh "${SSHOPTS[@]}" "$PI" 'sudo mkdir -p /usr/local/lib/ritornello/plugins /etc/ritornello'

# The service runs unprivileged: a dedicated system user, created on first
# deployment (device access comes through the groups declared in the unit,
# not through useradd -G). Its home is the state directory, the only place
# a subprocess (mpv) could want to write to.
ssh "${SSHOPTS[@]}" "$PI" 'id -u ritornello >/dev/null 2>&1 \
  || sudo useradd --system --home-dir /var/lib/ritornello --no-create-home \
       --shell /usr/sbin/nologin ritornello'

# Prior `rm -rf` of the staging areas: if a previous deployment failed
# between the scp and the installation, `scp -r` into a leftover directory
# would create /tmp/locales/locales, and a stray subdirectory would end up
# in /etc/ritornello/locales.
ssh "${SSHOPTS[@]}" "$PI" 'sudo mkdir -p /etc/ritornello/locales && rm -rf /tmp/locales'
scp "${SSHOPTS[@]}" -r deploy/locales "$PI:/tmp/locales"
ssh "${SSHOPTS[@]}" "$PI" 'sudo cp -r /tmp/locales/. /etc/ritornello/locales/ && rm -rf /tmp/locales'

ssh "${SSHOPTS[@]}" "$PI" 'sudo mkdir -p /etc/ritornello/input-presets && rm -rf /tmp/input-presets'
scp "${SSHOPTS[@]}" -r deploy/input-presets "$PI:/tmp/input-presets"
ssh "${SSHOPTS[@]}" "$PI" 'sudo cp -r /tmp/input-presets/. /etc/ritornello/input-presets/ && rm -rf /tmp/input-presets'

# Default configuration, provisioned from the example files ONLY when the
# target file is absent: a first installation works without any manual
# copy, and an existing configuration (stations added from the browser,
# learned bindings) is never overwritten. These two files hold what the
# user produced, so nothing here has any business completing them.
scp "${SSHOPTS[@]}" deploy/stations.example.toml \
  deploy/input-bindings.example.toml "$PI:/tmp/"
ssh "${SSHOPTS[@]}" "$PI" 'for f in stations input-bindings; do
  [ -e "/etc/ritornello/$f.toml" ] || sudo cp "/tmp/$f.example.toml" "/etc/ritornello/$f.toml"
  rm -f "/tmp/$f.example.toml"
done'

# plugins.toml is the one configuration file the deployment also COMPLETES
# instead of merely provisioning. It is not user data: it says which of the
# binaries just installed the core is to launch. An entry missing there
# means a plugin shipped and never started — silently, and for as long as
# nobody happens to read the documentation. Every plugin added since a
# device went into service (the `files` source, `radiofrance-metas`, the
# metadata plugins split out of `cd`) needed a hand-written entry on that
# device to exist at all, a step documented three times over precisely
# because it kept being missed.
#
# Only the blocks whose `name` is absent are appended, never a rewrite: a
# hand-edited exec (the mce -> generic-input migration), a metadata chain
# reordered on purpose and any locally added plugin all survive untouched.
# What this cannot read is intent — a plugin deliberately deleted from the
# file comes back on the next deployment — so what gets appended is named
# on the console rather than applied in silence.
scp "${SSHOPTS[@]}" deploy/plugins.example.toml deploy/missing-plugins.awk "$PI:/tmp/"
ssh "${SSHOPTS[@]}" "$PI" 'set -e
  if [ -e /etc/ritornello/plugins.toml ]; then
    awk -f /tmp/missing-plugins.awk /etc/ritornello/plugins.toml \
      /tmp/plugins.example.toml > /tmp/plugins.ajouts
    if [ -s /tmp/plugins.ajouts ]; then
      sudo tee -a /etc/ritornello/plugins.toml < /tmp/plugins.ajouts > /dev/null
      echo "plugins.toml completed with:$(sed -n "s/^name = \"\(.*\)\"/ \1/p" \
        /tmp/plugins.ajouts | tr -d "\n")"
    fi
  else
    sudo cp /tmp/plugins.example.toml /etc/ritornello/plugins.toml
    echo "plugins.toml provisioned from the defaults"
  fi
  rm -f /tmp/plugins.example.toml /tmp/missing-plugins.awk /tmp/plugins.ajouts'

scp "${SSHOPTS[@]}" "$OUT/ritornello-core" "$PI:/tmp/ritornello-core"
scp "${SSHOPTS[@]}" "${PLUGINS[@]/#/$OUT/ritornello-plugin-}" "$PI:/tmp/"
scp "${SSHOPTS[@]}" deploy/ritornello.service deploy/50-ritornello-power.rules "$PI:/tmp/"

# After every copy into /etc/ritornello, which would hand them back to
# root: the directory belongs to the service, because the radio and
# generic-input plugins persist stations.toml and input-bindings.toml
# there through atomic writes (.tmp then rename), which requires write
# access to the directory itself. /var/lib/ritornello is taken over too:
# a previous root installation left state files there that the service
# could no longer rewrite.
ssh "${SSHOPTS[@]}" "$PI" 'sudo chown -R ritornello: /etc/ritornello \
  && if [ -d /var/lib/ritornello ]; then sudo chown -R ritornello: /var/lib/ritornello; fi'

# The mount binary of the `files` source. It lands OUTSIDE the plugins
# directory on purpose: the core launches everything it finds there, and this
# one is not launched by the core but by systemd, as root.
scp "${SSHOPTS[@]}" "$OUT/ritornello-media-mount" "$PI:/tmp/ritornello-media-mount"
scp "${SSHOPTS[@]}" deploy/ritornello-media-mount.service deploy/51-ritornello-media.rules "$PI:/tmp/"
ssh "${SSHOPTS[@]}" "$PI" 'sudo install -m 0755 -o root -g root \
    /tmp/ritornello-media-mount /usr/local/lib/ritornello/ritornello-media-mount \
  && sudo mkdir -p /etc/polkit-1/rules.d \
  && sudo install -m 0644 -o root -g root \
    /tmp/ritornello-media-mount.service /etc/systemd/system/ \
  && sudo install -m 0644 -o root -g root \
    /tmp/51-ritornello-media.rules /etc/polkit-1/rules.d/ \
  && rm -f /tmp/ritornello-media-mount /tmp/ritornello-media-mount.service \
    /tmp/51-ritornello-media.rules'

# Mount points and credentials. The mount point of a share is imposed
# (/mnt/ritornello/<name>), never read from the configuration. The credentials
# directory belongs to the service — the page writes a <name>.cred file there
# when a share is declared — and is readable by nobody else; the root binary,
# for its part, reads everything.
ssh "${SSHOPTS[@]}" "$PI" 'sudo mkdir -p /mnt/ritornello /etc/ritornello/media-credentials \
  && sudo chown ritornello: /etc/ritornello/media-credentials \
  && sudo chmod 0700 /etc/ritornello/media-credentials'

# Enabled, not started: the unit is a `oneshot` that reconciles the declared
# shares, and what it is enabled for is the boot of the machine. The plugin
# starts it on demand the rest of the time.
ssh "${SSHOPTS[@]}" "$PI" 'sudo systemctl daemon-reload \
  && sudo systemctl enable ritornello-media-mount.service'

DEPLACE_PLUGINS=$(printf '/tmp/ritornello-plugin-%s ' "${PLUGINS[@]}")
ssh "${SSHOPTS[@]}" "$PI" "sudo mv /tmp/ritornello-core /usr/local/bin/ritornello-core \
  && sudo mv $DEPLACE_PLUGINS /usr/local/lib/ritornello/plugins/ \
  && sudo chmod +x /usr/local/lib/ritornello/plugins/* \
  && sudo rm -f /usr/local/lib/ritornello/plugins/ritornello-plugin-mce \
  && sudo mv /tmp/ritornello.service /etc/systemd/system/ \
  && sudo mkdir -p /etc/polkit-1/rules.d \
  && sudo mv /tmp/50-ritornello-power.rules /etc/polkit-1/rules.d/ \
  && sudo chown root: /etc/polkit-1/rules.d/50-ritornello-power.rules \
  && sudo chmod 644 /etc/polkit-1/rules.d/50-ritornello-power.rules \
  && sudo systemctl daemon-reload \
  && sudo systemctl enable ritornello \
  && sudo systemctl restart ritornello \
  && systemctl status ritornello --no-pager"
echo "OK — logs: ssh $PI journalctl -u ritornello -f"
