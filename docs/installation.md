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

    sudo apt install mpv cd-discid eject cifs-utils
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

`cifs-utils` provides `mount.cifs`, which the `files` source needs to mount
a network share; it is only useful if you intend to play files from a NAS
(see [Network shares](#network-shares)).

On **DietPi**, same packages (`sudo apt install mpv cd-discid eject
cifs-utils`), and two differences to know about:

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

    sudo apt install mpv cd-discid eject cifs-utils

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

Configuration: `deploy.sh` provisions `stations.toml` and
`input-bindings.toml` from the `deploy/*.example.toml` defaults **only
when the file is absent** — a first installation needs no manual copy,
and a file that exists is **never overwritten**, whatever it contains.
Those two hold what you produced (stations added from the browser,
learned bindings), so nothing may complete them.

`plugins.toml` is the exception, because it holds no such thing: it lists
which of the binaries just installed the core is to launch. It is
provisioned the same way when absent, and otherwise **completed in
place** — the entries of `deploy/plugins.example.toml` whose `name` the
file does not already declare are appended, and the script prints which
ones. Everything already there is left alone: a hand-edited `exec`, a
metadata chain reordered on purpose, a plugin of your own. So an update
that introduces a plugin no longer needs an edit on the device — but a
plugin you deleted from the file on purpose comes back, and appended
`metadata` entries land at the end of the chain, hence last in priority
(see [plugins.md](plugins.md)).

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
| firmware under-voltage flag (`/dev/vcio`, read-only) | `video` group (see below) |

The groups above the last row are granted by `SupplementaryGroups=` in the
unit — the user itself is not added to any group, so the unit is the single
place to audit. `video` is the deliberate exception: `deploy.sh` runs
`sudo usermod -aG video ritornello` instead, because the whole privilege
fits in that one line and `/dev/vcio` already has the right permissions
(`crw-rw---- root video`) — no udev rule to install, nothing to add to the
hardened unit for it. It grants read access to the firmware's mailbox,
which `vcgencmd get_throttled` uses: the kernel publishes no sysfs or procfs
equivalent (`find /sys -name "*throttled*"` finds nothing on a real Pi, only
`soc:firmware:vcio` shows up), so this is the only way to ever learn that an
under-voltage episode has occurred since boot. Without it,
`under_voltage_since_boot` in `GET /api/system` stays `null` and the System
tab shows "—" for it, the same as any other sensor a machine does not
expose — nothing else breaks. `/etc/ritornello` is owned by the service
user: the radio and generic-input plugins persist `stations.toml` and
`input-bindings.toml` there through atomic writes (`.tmp` then rename),
which requires write access to the directory itself.

Installing by hand instead of through `deploy.sh`? The two commands the
script runs for this are:

    sudo useradd --system --home-dir /var/lib/ritornello --no-create-home \
      --shell /usr/sbin/nologin ritornello
    sudo chown -R ritornello: /etc/ritornello
    sudo usermod -aG video ritornello

An installation deployed before this change ran as root: the next
`deploy.sh` migrates it (the user is created, `/etc/ritornello` and
`/var/lib/ritornello` change owner, the new unit replaces the old one).

## Shutdown and reboot from the web UI

The System tab offers three power actions. Two of them act on the machine
and need an authorisation; the third needs none. Like every other route of
the appliance, these routes carry no authentication: anyone who can reach
port 8080 can power the machine off. This is the accepted design of this
project, not an oversight to fix — the appliance is meant to sit on a
trusted network, the same way its other routes do. A cross-origin HTML form
cannot reach them regardless, though: the request body is JSON, and a plain
HTML form has no way to set the `content-type: application/json` header
the endpoint requires.

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
`-ignore-inhibit` when an inhibitor is held — which also means a confirmed
shutdown overrides shutdown inhibitors and will not wait for an
in-progress `apt`/`dpkg` run to finish.

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
it back, and because the restart works by exiting the process, it leaves
mpv and the plugin processes behind: only systemd's cgroup sweeps them when
it manages the unit. Left running, they keep holding their sockets in
`/run/ritornello` and the ALSA device, which makes the next manual start
fail confusingly. And systemd's start rate limit applies: five restarts
within ten seconds leave the unit failed, cleared with
`sudo systemctl reset-failed ritornello`.

## Network shares

The `files` source plays audio files from a folder of the device or from
an SMB share. Only the share needs anything installed:

    sudo apt install cifs-utils smbclient

`cifs-utils` provides `mount.cifs`, without which a share is declared but
never mounts; `smbclient` is **optional** — it is what lets the page browse
a share *before* declaring it, and without it that wizard is greyed out
while manual entry keeps working. [plugins.md](plugins.md) says what each
one degrades when absent.

Beyond those packages, a share needs the two files `deploy.sh` puts in
place —
`/etc/systemd/system/ritornello-media-mount.service` and
`/etc/polkit-1/rules.d/51-ritornello-media.rules`. The script also creates
`/mnt/ritornello` and `/etc/ritornello/media-credentials` (mode `0700`,
owned by the service), and enables the mount unit so shares come back
after a reboot. On a device already in service, the `files` entry of
`plugins.toml` is appended by the same run — see [plugins.md](plugins.md).

**Declaring a share** happens in the browser, at
`http://<host>:8080/plugins/files/`. Give the server address, connect, and
the wizard lists the shares it exposes; pick one, walk down to the folder
you want, and confirm. Nothing is mounted until you do.

You are not asked to name anything. The internal name is derived from the
share and de-duplicated, because it becomes both a directory name and a
credentials filename — deriving it guarantees a valid one, where typing it
allowed a refusal with no way to see why.

Confirming writes `/etc/ritornello/media-roots.toml` and
`/etc/ritornello/media-credentials/<name>.cred`, then asks systemd to run
the mount unit on its own. The mount point is not yours to pick: it is
always `/mnt/ritornello/<name>`.
`deploy/media-roots.example.toml` documents the file for the rare case of
editing it by hand.

The service does not mount anything itself — it is unprivileged, with
`NoNewPrivileges=true`. It asks systemd to start
`ritornello-media-mount.service`, a `oneshot` running as root that
reconciles the declared shares (mounts what is missing, unmounts what is
no longer declared). Why the boundary is drawn there, and what the root
side revalidates, is in [plugins.md](plugins.md).

**A refused mount** shows on the page with `systemctl`'s own error output,
copied verbatim. A polkit refusal reads as such — "Interactive
authentication required", or "Access denied" — and means the rule is
missing or did not land: reinstall `51-ritornello-media.rules` into
`/etc/polkit-1/rules.d/` (a `deploy.sh` run does it), and check polkit
itself is installed (see the previous section — it is absent by default on
DietPi). There is no capability probe here, unlike the power buttons:
systemd offers no "CanStartUnit" equivalent to logind's `CanPowerOff`, so
the plugin tries and reports. To check by hand, on the device:

    sudo -u ritornello systemctl start ritornello-media-mount.service
    journalctl -u ritornello-media-mount -n 30

Any other error — bad password, unreachable host, `mount.cifs` missing —
appears in that same journal, one line per share, since a share that fails
does not fail the whole unit.

**One point to verify on the target machine.** The mount unit itself is
deliberately left unhardened, so that it mounts in the host's own
namespace. `ritornello.service`, on the other hand, *is* hardened
(`ProtectSystem=strict`, `ProtectHome=true`) and therefore runs in a mount
namespace of its own. systemd mounts that namespace `rslave`, which
*should* make mounts made later by the host visible inside it — expected
behaviour, not something measured on this hardware yet. If a share mounts
(the unit's journal says so) while the plugin keeps seeing an empty
`/mnt/ritornello/<name>`, that propagation is the suspect, and the recourse
is a `BindPaths=/mnt/ritornello` in `ritornello.service`. A second point to
confirm against the NAS in use: no SMB dialect is forced (`vers=` is
deliberately left out, the kernel's negotiation ageing better than a
pinned version).

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
