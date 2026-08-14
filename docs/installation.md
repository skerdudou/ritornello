# Installation and operations

## Portability

Nothing in the code is specific to the Raspberry Pi: the remote control
goes through `evdev` (the generic Linux input API, not GPIO), sound through
ALSA/mpv, IPC through Unix sockets — all of which run on any Linux, x86_64
and ARM alike. The Pi 2 is this project's historical reference hardware,
not a technical constraint — the examples below merely illustrate it.

## Building

The web interface is a SPA (Vue 3 + shadcn-vue) embedded into the core
binary: **Node 20+** is therefore a development prerequisite, where `cargo`
used to be enough. The reference procedure is `deploy/build.sh`, which
always runs the three steps in this order:

    ./deploy/build.sh                 # npm, then cargo x86_64, then cross ARM
    TARGET=aarch64-unknown-linux-gnu ./deploy/build.sh

The npm build runs only once: its output is read at compile time by both
cargo steps. This is what lets `cross` work with a Docker image that has no
Node.

A `cargo build` run on its own, without building the UI first,
**succeeds**: a placeholder is embedded instead, and the served page
invites you to run `npm run build --workspaces`. This is not a failure.
The tests (`cargo test --workspace`) stay green in that situation; on the
browser side, `npm test --workspaces` covers the UI and `npm run e2e -w app`
the full journeys (see [development.md](development.md)).

The workspace compiles natively for the architecture of the machine
running the command (x86_64 on a typical Linux PC/server), and for ARM by
cross-compilation with [`cross`](https://github.com/cross-rs/cross) (which
needs Docker):

    # Native (e.g. x86_64) — also used for tests during development
    cargo build --workspace
    cargo test --workspace

    # ARM cross-compilation (e.g. Raspberry Pi 2, 32-bit)
    cargo install cross --locked
    cross build --release --workspace --target armv7-unknown-linux-gnueabihf

Both paths are exercised on every project change. Other ARM targets are
possible with `cross`: `aarch64-unknown-linux-gnu` (64-bit ARM boards, Pi
3/4/5 class — untested on this project for lack of hardware, but with no
reason not to work).

## Example: Raspberry Pi 2

Two distributions are exercised on this project's hardware: Raspberry Pi
OS Lite and DietPi. Both work identically for everything that matters
(Debian base, systemd, same packages, same `deploy.sh`, same unit) — only
the initial tuning tools differ.

On **Raspberry Pi OS Lite**:

    sudo apt install mpv cd-discid eject
    # analog jack as default output + hardware volume at maximum
    sudo raspi-config nonint do_audio 1
    amixer set PCM 100%

No configuration to copy: on first deployment, `deploy.sh` provisions
`/etc/ritornello` with the defaults (all bundled plugins, two starter
stations, MCE remote bindings — the `deploy/*.example.toml` files), then
everything is adjusted from the browser or by editing those files. An
existing configuration is never overwritten (see
[Deploying](#deploying)).

Wifi: `sudo raspi-config` (System Options > Wireless LAN).

On **DietPi**, same packages (`sudo apt install mpv cd-discid eject`),
and two differences to know about:

- no `raspi-config`: the sound card is **enabled and picked** through
  `dietpi-config` (Audio Options) — DietPi ships with onboard sound
  disabled, so this step is required before anything plays; `amixer`
  comes with `alsa-utils` if missing;
- mDNS is not installed by default: target the device by IP, e.g.
  `PI=dietpi@192.168.1.20 ./deploy/deploy.sh` (the `dietpi` and `root`
  users both work — the script's `sudo` calls are a no-op for root), or
  install `avahi-daemon` to keep using a `<hostname>.local` name.

## Example: generic x86_64 Linux machine

Same packages, minus the Pi-specific steps (no `raspi-config`, the audio
output is picked directly through `/api/audio-output`):

    sudo apt install mpv cd-discid eject

Configuration is provisioned by `deploy.sh` here too (see above).
`deploy/deploy.sh` works identically: `TARGET=x86_64-unknown-linux-gnu
PI=user@host ./deploy/deploy.sh` (no need for `cross`/Docker for this
target if the build machine is already x86_64 — a native `cargo build` is
enough then; `cross` is mostly useful for changing architecture).

## Deploying

    PI=pi@raspberrypi.local ./deploy/deploy.sh

`PI` names any target SSH host (Pi or other Linux), and `TARGET` the
compilation target (see [Building](#building)) — the two override
independently, e.g. `TARGET=x86_64-unknown-linux-gnu PI=user@host
./deploy/deploy.sh`. The script chains `build.sh` (so the npm UI build
**then** the cross-compilation — the order guarantees the embedded SPA is
fresh), copies the binaries, the language packs and the presets, installs
the systemd unit and restarts the service.

Even without an SSH key, the password is asked **once** per run, not once
per copy: every ssh/scp call of the script shares a single master
connection (`ControlMaster`), closed when the script exits. To not type
it at all, install a key once — `ssh-keygen` if you have none, then
`ssh-copy-id pi@raspberrypi.local`.

Web interface: http://<host>:8080 — logs: `journalctl -u ritornello -f`.

Configuration: `deploy.sh` provisions `plugins.toml`, `stations.toml` and
`input-bindings.toml` from the `deploy/*.example.toml` defaults **only
when the file is absent** — a first installation needs no manual copy,
and a file that exists is **never overwritten**, whatever it contains.
The flip side of that guarantee: when an update introduces new plugins,
their entries must be added to the existing `plugins.toml` by hand (see
[plugins.md](plugins.md)).

## Unprivileged service

The service does not run as root. Nothing in the code needs root — only
device access, which comes through groups. `deploy.sh` creates a
dedicated `ritornello` system user on first deployment, and the systemd
unit (`deploy/ritornello.service`) grants the groups and applies the
usual hardening (`NoNewPrivileges`, `ProtectSystem=strict`,
`ProtectHome`):

| Access | How |
|---|---|
| HTTP port 8080 | nothing needed (unprivileged port) |
| sound (ALSA/mpv) | `audio` group |
| remote control (`/dev/input/*`) | `input` group |
| CD drive (`/dev/sr0`, `eject`) | `cdrom` group |
| HDMI console (`/dev/tty1`) | `tty` group |
| OS shutdown / reboot | polkit rule + logind (see the next section) |
| plugin and mpv sockets (`/run/ritornello`) | `RuntimeDirectory` |
| persisted state (`/var/lib/ritornello`) | `StateDirectory` |

The groups are granted by `SupplementaryGroups=` in the unit — the user
itself is not added to any group, so the unit is the single place to
audit. `/etc/ritornello` is owned by the service user: the radio and
generic-input plugins persist `stations.toml` and `input-bindings.toml`
there through atomic writes (`.tmp` then rename), which requires write
access to the directory itself.

Installing by hand instead of through `deploy.sh`? The two commands the
script runs for this are:

    sudo useradd --system --home-dir /var/lib/ritornello --no-create-home \
      --shell /usr/sbin/nologin ritornello
    sudo chown -R ritornello: /etc/ritornello

An installation deployed before this change ran as root: the next
`deploy.sh` migrates it (the user is created, `/etc/ritornello` and
`/var/lib/ritornello` change owner, the new unit replaces the old one).

## Shutdown and reboot from the web UI

The System tab offers three power actions. Two of them act on the machine
and need an authorisation; the third needs none.

| Action | Mechanism | Prerequisite |
|---|---|---|
| Shut down / restart the **system** | `systemctl poweroff` / `reboot` → logind → polkit | the polkit rule below |
| Restart **Ritornello** | the process exits, systemd starts it again (`Restart=always` in the unit) | none |

`deploy.sh` installs `deploy/50-ritornello-power.rules` into
`/etc/polkit-1/rules.d/`. It grants the `ritornello` user the six logind
actions involved — power-off and reboot, each in its plain,
`-multiple-sessions` and `-ignore-inhibit` form. All six, because logind
checks the plain action only when nothing else is going on: it switches to
`-multiple-sessions` as soon as another session exists (an open SSH
connection is enough, which is the usual situation while testing) and to
`-ignore-inhibit` when an inhibitor is held.

polkit itself is not installed by `deploy.sh` — the script installs no
package — and it is not present everywhere:

- **DietPi**: absent by default, `sudo apt install polkitd`;
- **Raspberry Pi OS Lite**: normally already there; if not, same command;
- **other Debian-based distributions**: `polkitd`, or `policykit-1` before
  Debian 12;
- **Arch, Fedora, openSUSE**: `polkit`, generally already installed.

To check, on the device:

    sudo -u ritornello busctl --system call org.freedesktop.login1 \
      /org/freedesktop/login1 org.freedesktop.login1.Manager CanPowerOff

`s "yes"` means the rule is in effect. `s "challenge"` or `s "no"` means it
is not: polkit is missing, or the rule did not land.

Nothing breaks without it: the core asks logind the same question at
startup, and the two system buttons stay **disabled**, with the reason shown
on the page. That answer is cached for the lifetime of the process, so
installing polkit takes effect at the next service start —
`sudo systemctl restart ritornello`, or simply the next `deploy.sh`.

"Restart Ritornello" depends on none of this: the process exits and systemd
starts it again two seconds later. Run **outside** systemd (development),
the same action merely stops the process — there is no supervisor to bring
it back. And systemd's start rate limit applies: five restarts within ten
seconds leave the unit failed, cleared with
`sudo systemctl reset-failed ritornello`.

## Audio dropouts

Two distinct buffers protect playback, and they do not address the same
problem. Confusing them wastes time.

| Variable | Default | What it protects |
|---|---|---|
| `RITORNELLO_AUDIO_BUFFER` | `0.2` | the **output**: an ALSA write deadline missed because the machine was busy |
| `RITORNELLO_NETWORK_READAHEAD` | `1` | the **input**: network jitter draining an internet stream's read-ahead |

Both are in seconds and apply when mpv is launched (`--audio-buffer` and
`--demuxer-readahead-secs`). The defaults are **mpv's own**: with no
variable set, playback behaves exactly as if these options were not passed.
An unreadable or out-of-range value is ignored with a warning in the logs,
without preventing startup.

Before turning a knob, **identify which one** — the two symptoms sound the
same but are not fixed in the same place:

    mpv --no-video --msg-level=ao=v,cache=v <station-url> 2>&1 \
      | grep -iE "underrun|buffering|cache"

`underrun` lines point at the output: raise `RITORNELLO_AUDIO_BUFFER`, for
example to `0.5`. `buffering` lines point at the input: raise
`RITORNELLO_NETWORK_READAHEAD`, for example to `10`, or even `30` on a
flaky link — ten seconds of 128 kbit/s MP3 weigh about 160 KB, negligible
even with 1 GB of RAM.

One case to rule out from the start: during development under **WSL**,
audio crosses the WSLg bridge to Windows, whose own jitter produces
dropouts that neither of these settings will fix. Only draw conclusions
about the buffers after listening on the target machine.

Increasing the **output** buffer helps against dropouts caused by machine
load, at the cost of the same amount of latency on volume or mute taking
effect. **Reducing it makes dropouts worse**: it is the direction of the
change that matters, not its magnitude.

To tell the two causes apart, run `journalctl -u ritornello -f` during a
dropout: mpv logs the network cache draining, not ALSA underruns.
