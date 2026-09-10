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

## Installing from a release

Pushing a tag `vX.Y.Z` builds a **draft** GitHub release carrying prebuilt
archives, one per architecture and per component:
`ritornello-core-<version>-<arch>.tar.gz`, `ritornello-plugins-<version>-<arch>.tar.gz`
(every bundled plugin together) and one
`ritornello-plugin-<name>-<version>-<arch>.tar.gz` per plugin, for installing
or upgrading a single one. `<version>` is not one number shared by the whole
release: the core's archive and each plugin's own archive carry **that
component's own** version, which moves only when that component changes,
while the all-plugins bundle carries the release's own number. There is
nowhere else to look it up — read it where it already sits, in the name of
the file attached to the release page. A single `SHA256SUMS` covers every
archive of the release, whatever the architecture. This is an alternative to
`deploy.sh`, not a replacement for it: `deploy.sh` still builds from source
over SSH and remains the development path (see [Deploying](#deploying)
below); a release archive is for putting a specific tagged version onto a
device with no build toolchain at all.

Three different numbers are at play here, and they answer three different
questions. The **product number** — `vX.Y.Z`, the git tag — names the
release and carries the generation: `0.2.7` is the seventh delivery of the
`0.2` generation. Each shipped component (the core, each plugin) declares
**its own** patch version, so a fix confined to one plugin does not
renumber everything else and does not make the updater think ten unrelated
components changed too. `PROTOCOL_VERSION`, the wire-compatibility contract
between the core and a plugin (see [plugins.md](plugins.md)), is a third
number again, and it moves only on a breaking change to that wire format —
not on every release, and not with every component's own patch bumps. Major
and minor are kept identical everywhere — the product number and every
component's own version — so only the patch digit is ever free, component
by component.

The release gesture, then: bump the version of whichever component you
changed, and tag with the next product number. The workflow publishes
exactly the components whose declared version moved since the previous
published release (`scripts/changed-components.sh`, run from a development
machine against an arbitrary ref — see [What has not been
verified](#what-has-not-been-verified) below for how much of this has
actually been exercised). Forgetting to bump a component's version is not a
silent no-op: that component ships nothing this release, and if *no*
component moved the script exits 2 and fails the job loudly rather than
publishing an empty release that looks like success.

**One case is a silent no-op, and it is the one to know about.** A change to
a shared crate (`ritornello-proto`, `ritornello-i18n`,
`ritornello-plugin-sdk`, `ritornello-updater`) makes the script republish
*every* component — correctly, because all eleven binaries were rebuilt — but
under their **unchanged** version numbers. A device decides what to install
by comparing versions and nothing else, so it sees every row as up to date
and fetches none of the new archives. The release looks complete and delivers
nothing.

**For a shared-crate change to reach devices, bump the version of every
component it actually reaches, in the same commit.** For `ritornello-proto`,
`ritornello-i18n` and `ritornello-plugin-sdk` that is all eleven: they are
linked into every binary. For `ritornello-updater` it is **the core alone** —
its binary is not linked into anything and ships only inside the core's
archive, so bumping the ten plugins for it would deliver ten identical
archives. The script republishes everything in either case, because
over-publishing is the safe direction and one mechanism beats two for a crate
that changes this rarely; what you choose is which versions to move.

The script prints all of this on stderr when it detects such a change. It does
not refuse the release, because republishing is still the right thing to
build — it is only not, on its own, delivering.

Detection reads a single page of the GitHub releases API — one hundred
releases (`per_page=100`). A component that has not shipped a new archive of
its own within the last hundred deliveries would drop off that page and out
of the catalogue: distant at this project's pace, but not impossible, and
worth knowing about rather than discovering it the day it happens.

A published release must never be deleted, nor its attached files removed.
The archive of a component that has not changed in a long time lives in the
release where it last changed, and that is where the device installs or
repairs it from.

**This is an upgrade path, not a fresh install.** The archives carry
binaries, units, polkit rules and language packs — nothing else. They do not
create the `ritornello` system user, install `mpv`/`cd-discid`/`eject`/
`cifs-utils`, create `/var/lib/ritornello`, `/mnt/ritornello` or
`/etc/ritornello/media-credentials`, nor enable any unit. Extracting them
onto a virgin machine leaves a device that cannot start, and the very first
command below (`chown -R ritornello:`) fails outright for want of that user.
Prepare the device once as [Example: Raspberry Pi 2](#example-raspberry-pi-2)
describes — the packages, the audio, then a first `deploy.sh` that provisions
`/etc/ritornello`, creates the user and enables the units — and use release
archives from then on.

The common case — the core and every plugin, from a fresh download. The
release's own notes carry the same recipe; since the release is published as
a **draft**, whoever reviews it can fill `<version>` and `<arch>` in before
publishing, and the template ships them as placeholders:

    sha256sum -c SHA256SUMS --ignore-missing
    sudo tar --no-same-owner -C / -xzf ritornello-core-<version>-<arch>.tar.gz
    sudo tar --no-same-owner -C / -xzf ritornello-plugins-<version>-<arch>.tar.gz
    sudo chown -R ritornello: /etc/ritornello
    sudo systemctl daemon-reload && sudo systemctl restart ritornello

`--no-same-owner` is belt and braces rather than a workaround. The archives
are produced with every entry owned by `root` and `scripts/package-release.sh`
refuses to emit one that is not — but extracting as root restores whatever
ownership an archive happens to carry, so the flag also protects an operator
handed an older archive. What it protects: `/etc/polkit-1/rules.d/*.rules`
are JavaScript that `polkitd` evaluates as root, and a rules file writable by
a non-root uid is a local privilege escalation. The same reasoning covers
`/usr/local/bin/ritornello-core`, the plugin binaries, the root-run
`ritornello-media-mount` helper and the systemd units — which is why
`deploy.sh` installs every one of them `-o root -g root`.

Replacing a single plugin (an upgrade, or a fix confined to one binary) is
the same two commands with that plugin's own archive in place of the bundle.

One plugin needs a unit enabled by hand the first time it is installed from
an archive — `files`, whose archive carries
`ritornello-media-mount.service`:

    sudo systemctl enable ritornello-media-mount.service

Enabled, **not** started: what it is enabled for is machine boot, where it
reconciles the declared network shares (see [Network
shares](#network-shares)). `deploy.sh` does this for you; an archive cannot,
so a `files` plugin installed from a release and never enabled this way stops
reconciling shares at the next reboot, in silence.

### Enabling automatic updates (once, by hand)

An update can replace binaries and locale catalogs. It can never write a
systemd unit or a polkit rule — that is what stops a forged archive from
gaining root, and it is why this feature's own installation is manual.

**On a device deployed with `deploy.sh`, it is not:** the script installs
the four files below itself, alongside the two polkit rules and the units
it already placed. Deployment over SSH is a privileged gesture by nature;
an update is not, and that asymmetry is the whole point. What follows is
for a device installed from release archives.

From a release archive of the core, extracted as described above, four
files are new:

    sudo tar --no-same-owner -C / -xzf ritornello-core-<version>-<arch>.tar.gz \
      ./usr/local/lib/ritornello/ritornello-update \
      ./etc/systemd/system/ritornello-update.service \
      ./etc/systemd/system/ritornello-rollback.service \
      ./etc/polkit-1/rules.d/52-ritornello-update.rules

The updated `ritornello.service` carries the start limit and the
`OnFailure=` line that arm the rollback, so install it too, then:

    sudo systemctl daemon-reload
    sudo systemctl restart ritornello

Without the polkit rule the page still checks and still reports, and every
install fails with `systemctl`'s own refusal, shown verbatim in the card:
`Access denied`, or `Interactive authentication required`. **It does not name
the rule that is missing** — systemctl knows nothing about which `.rules` file
would have granted the action — so if an install refuses with either of those
two sentences and this file has not been installed, that is the cause. (A
missing *unit* is a different failure and does name its file: `Unit
ritornello-update.service not found`.)
Without the `OnFailure=` line everything works and there is no safety net.

**Why a blind `sudo tar -C /` cannot clobber a configuration.** Each
archive's tree holds files only at the exact path they occupy on the
device — the binary, its systemd unit, its polkit rule, its language packs —
and never `stations.toml`, `input-bindings.toml` or `plugins.toml`, the
three files that hold what an operator produced (stations added from the
browser, bindings learned, which plugins to launch). Those are structurally
absent from the tree, the same guarantee `deploy.sh` gives by never
overwriting a file that already exists (see [Deploying](#deploying)): there
is nothing to guard against, because there is nothing there to overwrite.

What an archive carries **beside** its tree, to be copied by hand rather
than extracted onto the device: the example config for the plugins that
have one (`stations.example.toml`, `media-roots.example.toml`, and so on),
and — for a single plugin's archive — `plugins.toml.fragment`, the
`[[plugin]]` block that `deploy/plugins.example.toml` would otherwise carry
for it. A plugin installed on its own has no other way to tell the core to
launch it: appending that fragment to `/etc/ritornello/plugins.toml` is what
actually starts the new binary (see [Declaring the
plugins](plugins.md#declaring-the-plugins)).

Three architectures are built for every release: `armv7` (Raspberry Pi 2
and similar, the reference hardware), `x86_64` (this project's own test and
end-to-end target, exercised on every commit rather than merely
cross-compiled) and `arm64` (Pi 3/4/5 class) — cross-compiled on every
release but **never started on real hardware**, for lack of a device to try
it on.

### Publishing a prerelease

For trying a redeployment before it reaches anyone, or for offering an edge
channel to whoever wants it. A prerelease is an ordinary release in every
respect but one flag, so the path it exercises is the production path: the
same archives, the same `SHA256SUMS`, the same rollback net.

**The shape of the tag decides.** A tag carrying a semver prerelease
suffix — `v0.2.1-beta.1` — is created as a prerelease; anything else is a
finished release. There is no checkbox to forget and no second place where
the same intent is stated. Like any release it lands as a **draft** first,
so the notes are read before it can be installed by anything.

Two rules, and neither is a convention that can be bent:

1. **The tag equals the product number**, suffix included. So the beta is
   prepared by setting `[workspace.package] version` to `0.2.1-beta.1` and
   tagging `v0.2.1-beta.1`. A workflow step refuses a tag that disagrees.
2. **Every component the beta ships carries the beta's own number.** The
   device decides by version *equality*, never by order: a component
   shipped as `0.2.1` inside `v0.2.1-beta.1` and shipped again as `0.2.1`
   in the finished `v0.2.1` would look identical to a device that already
   has the beta's bytes, and the tester would keep the older binary for
   ever, silently. `version_coherence.rs` refuses a suffix that is not the
   product's — and refuses any suffix at all in a finished product, which
   is what stops a leftover `-beta.2` from riding into a real release.

The finished release then needs no special handling: its components differ
from the beta's, so every device installs them, testers included. That
holds because the "what changed" step measures against the last *finished*
release, so anything a beta shipped is shipped again by the delivery it
prepared.

On the device, prereleases are only ever *offered* to an owner who ticked
"Offer prereleases" (see
[interface.md](interface.md#prereleases-and-how-a-device-asks-for-them)).
The switch is off by default, and a device that has never been told
otherwise cannot be offered one.

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

On **DietPi**, one package more (`sudo apt install mpv cd-discid eject
cifs-utils polkitd`), and three differences to know about:

- polkit is **absent** on a DietPi image, where it is present on most other
  Debian-based systems — hence `polkitd` in the line above — **and
  `systemd-logind` is masked**, to save memory. Both are needed by the
  System tab's shut-down and restart buttons, and polkit alone is not
  enough: those two actions are logind methods, so unmask logind as well
  (`sudo systemctl unmask systemd-logind && sudo systemctl start
  systemd-logind`). Mounting a network share needs polkit only, never
  logind. None of this fails at install time — only once the device is in
  service. See [Shutdown and reboot from the web
  UI](#shutdown-and-reboot-from-the-web-ui);
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
the systemd units, the polkit rules and the privileged updater, and
restarts the service.

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
`PrivateTmp`, `ProtectHome`):

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
| a writable `/tmp` (mpv's PulseAudio probe wants one before falling back to ALSA) | `PrivateTmp` |
| firmware under-voltage flag (`/dev/vcio`, read-only) | `video` group |

Every group above is granted by `SupplementaryGroups=` in the unit — the user
itself is added to none, so the unit stays the single place to audit, and each
privilege belongs to *this service's processes* rather than to anything that
ever runs as `ritornello`. `video` is the newest of them: it grants read
access to the firmware's mailbox, `/dev/vcio`, which `vcgencmd get_throttled`
uses. The kernel publishes no sysfs or procfs equivalent (`find /sys -name
"*throttled*"` finds nothing on a real Pi, only `soc:firmware:vcio` shows up),
so this is the only way to ever learn that an under-voltage episode has
occurred since boot. No udev rule is needed: `/dev/vcio` already ships as
`crw-rw---- root video`. Without the group, `under_voltage_since_boot` in
`GET /api/system` stays `null` and the System tab shows "—" for it, the same
as any other sensor a machine does not expose — nothing else breaks.
`/etc/ritornello` is owned by the service
user: the radio and generic-input plugins persist `stations.toml` and
`input-bindings.toml` there through atomic writes (`.tmp` then rename),
which requires write access to the directory itself.

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

**A third answer means something else entirely**, and no polkit rule will
fix it:

    Call failed: Unit dbus-org.freedesktop.login1.service failed to load
    properly, please adjust/correct and reload service manager: File exists

That is not a refusal — it is the D-Bus **activation** of logind failing.
Nobody owns the `org.freedesktop.login1` name, so dbus tries to start
`dbus-org.freedesktop.login1.service`, the alias of `systemd-logind.service`,
and that load fails.

**On DietPi, the cause measured on the device is a masked unit** — the image
ships `systemd-logind` masked, to save memory:

    $ systemctl is-active systemd-logind; systemctl is-enabled systemd-logind
    inactive
    masked
    $ ls -l /etc/systemd/system/systemd-logind.service
    ... /etc/systemd/system/systemd-logind.service -> /dev/null

The repair, then a Ritornello restart because the probe is cached:

    sudo systemctl unmask systemd-logind
    sudo systemctl start systemd-logind
    systemctl is-enabled systemd-logind      # `enable` it if it says disabled
    sudo systemctl restart ritornello

Fix this **before** looking at polkit: shutting down and rebooting *are*
logind's own methods, so no polkit rule can help while logind is down. A
share mount is not affected — it goes through systemd's `manage-units`,
never logind, and works with polkit alone. The same `File exists` can also
come from a stray
`/etc/systemd/system/dbus-org.freedesktop.login1.service` competing with the
one in `/lib` (which is the alias shipped by the `systemd` package and
belongs there) — `sudo rm` then `sudo systemctl daemon-reload`. That second
case is reasoning, not something seen here.

Nothing breaks without any of it: the core asks logind the same question at
startup, and the two system buttons stay **disabled**, with the reason shown
on the page — and the page distinguishes the two causes, since they call for
different repairs (`logind_reachable` in
[interface.md](interface.md#system-page)). That answer is cached for the lifetime of the process, so
installing polkit takes effect at the next service start —
`sudo systemctl restart ritornello`, or simply the next `deploy.sh`.

"Restart Ritornello" depends on none of this: the process exits and systemd
starts it again two seconds later. Run **outside** systemd (development),
the same action merely stops the process — there is no supervisor to bring
it back, and because the restart works by exiting the process, it leaves
mpv and the plugin processes behind: only systemd's cgroup sweeps them when
it manages the unit. Their sockets are harmless — the core wipes and
recreates `{runtime_dir}/sockets` on every start (`prepare_sockets_dir` in
`plugins.rs`) — but left running, the orphaned processes keep holding the
ALSA device, the input devices, and the CD drive, which makes the next
manual start fail confusingly. And systemd's start rate limit applies: five restarts
within ten seconds leave the unit failed, cleared with
`sudo systemctl reset-failed ritornello`.

## Network shares

The `files` source plays audio files from a folder of the device or from
an SMB share. Only the share needs anything installed:

    sudo apt install cifs-utils smbclient

`cifs-utils` provides `mount.cifs`, without which a share fails to mount and
is refused rather than declared (see below); `smbclient` is **optional** —
it is what lets the page browse a share *before* declaring it, and without
it the wizard opens straight into manual entry, which keeps working just the
same. [plugins.md](plugins.md) says what each one degrades when absent.

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

**A refused mount** shows in the declaration popin, which stays open with
what was just typed still in it, and the share is not declared: the table
entry and the credentials file that were tentatively written are both
undone. The message carries `systemctl`'s own error output, copied
verbatim. A polkit refusal reads as such — "Interactive authentication
required", or "Access denied" — and means the rule is missing or did not
land: reinstall `51-ritornello-media.rules` into
`/etc/polkit-1/rules.d/` (a `deploy.sh` run does it), and check polkit
itself is installed (see the previous section — it is absent by default on
DietPi). There is no capability probe here, unlike the power buttons:
systemd offers no "CanStartUnit" equivalent to logind's `CanPowerOff`, so
the plugin tries and reports. To check by hand, on the device:

    sudo -u ritornello systemctl start ritornello-media-mount.service
    journalctl -u ritornello-media-mount -n 30

Any other error — bad password, unreachable host — appears in that same
log, one line per share, since a share that fails does not fail the
whole unit.

**A missing `cifs-utils` is checked before mounting**, and reported as
`mount.cifs not found in /sbin or /usr/sbin: install cifs-utils`. The check
exists because the error `mount` gives on its own names nothing useful:
`mount -t cifs` does not mount by itself, it hands over to `mount.cifs`,
the only side that reads a `credentials=` file. Without that program `mount`
calls mount(2) directly, nobody reads the option, the session opened is
anonymous — and the NAS refusing it surfaces as

    mount: /mnt/ritornello/music: cannot mount //192.168.1.15/music read-only.

which mentions neither authentication nor the missing package. Observed on
DietPi bookworm; if you meet that line on an older build, install
`cifs-utils` and start the unit again.

**One point to verify on the target machine.** The mount unit itself is
deliberately left unhardened, so that it mounts in the host's own
namespace. `ritornello.service`, on the other hand, *is* hardened
(`ProtectSystem=strict`, `ProtectHome=true`) and therefore runs in a mount
namespace of its own. systemd mounts that namespace `rslave`, which
*should* make mounts made later by the host visible inside it — expected
behaviour, not something measured on this hardware yet. If a share mounts
(the unit's log says so) while the plugin keeps seeing an empty
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

## What has not been verified

Self-update and plugin management were built and tested on a development
machine, never on the device they are meant to run on. This section names
each gesture, rather than leaving one blanket disclaimer that a reader could
mistake for caution instead of fact.

**Nothing has run on a Pi.** Installing, uninstalling and reordering a
plugin, and updating the core itself, all rewrite
`/etc/ritornello/plugins.toml` and ask the privileged
`ritornello-update.service` to place the files. None of the following has
been observed for real:

- `deploy.sh` placing the updater's binary, its two units and its polkit
  rule — the block exists and a test holds it to `packaging.toml`, but no
  deployment has run since it was written, so the first one is also its
  first trial;
- `systemctl start ritornello-update.service` running with the actual
  polkit rule in place;
- a running binary being replaced on disk while its own process is live;
- systemd restarting `ritornello.service` after an update, rather than a
  test process exiting on its own;
- the rollback firing;
- the note of what an install placed being written **before** the process
  leaves. On a device that write is followed by an exit that does not
  return, and the note is what stops a release that fails to start from
  being reinstalled every night; here the exit is a test closure that
  returns like any other, so the ordering is a property of the code's
  shape rather than something observed.

Every one of those is covered by unit and integration tests that fake the
privileged step; none is covered by the privileged step itself.

**The release workflow has never run**, on this repository or before this
project's own self-update work began — it fires only on a `git tag`, and no
tag has been pushed since. Consequently:

- "publish only what changed" is proven by running
  `scripts/changed-components.sh` by hand against a handful of refs on this
  checkout, and by reading `.github/workflows/ci.yml`, not by a real
  release. The first `git tag` pushed to this repository is the first real
  test of the whole workflow;
- `fetch-depth: 0` on the `publish` job is load-bearing and unproven:
  without it, the checkout has no tag history, `git show <ref>:...` finds
  nothing, `changed-components.sh` receives no previous ref, and the
  release publishes **every** component instead of only what changed. That
  failure mode is safe — nothing is lost or corrupted — but silent, and
  from the outside it looks exactly like the feature working;
- the guard added by this task, which refuses to draft a release that
  changed a systemd unit, a polkit rule or the updater without the notes
  saying "Action required" (`scripts/release-notes-guard.sh`), has been run
  by hand against commits of this repository, never inside the actual
  GitHub Actions job;
- nothing about the per-component versioning scheme itself — three
  archives named after three different numbers, a catalogue read from one
  page of a hundred releases — has been exercised by an actual device
  fetching an actual release.

**The rollback only recognises "does not start."** It watches the service
failing to come up — systemd's start-limit plus `OnFailure=` on the unit —
which is what a marker-and-restart scheme can cheaply detect. A core that
starts, stays up, and misbehaves quietly (a bad decode, a protocol
mismatch nobody wired for, a corrupted file it degrades under rather than
rejecting) triggers nothing at all. There is no health check beyond "the
process is alive."

**`arm64` has never started on real hardware.** It is cross-compiled on
every tagged release like the other two architectures (see
[Architectures](#installing-from-a-release) above), but nobody owns a
board of that class to try it on.

**The protocol-incompatibility refusal is proven only by tests.** The core
refuses a plugin whose announced `protocol` is not strictly equal to its
own `PROTOCOL_VERSION` (see [plugins.md](plugins.md)), but that number has
never actually moved in this project's history — there has been no real
wire break to refuse. The path is exercised by unit tests that fabricate a
mismatched announcement, not by an actually incompatible plugin built
against an older protocol.

**Known edges and debts in the code, recorded here rather than fixed or
dressed up as design:**

- The **four** places that rewrite `plugins.toml` on a live device — behind
  declaring a plugin, removing one, reordering the list, and the update
  worker appending the block of a plugin it has just installed — each read
  the file, transform the text and write it back, with no lock across the
  three steps. **An earlier version of this note claimed they were safe
  because none of them awaits anything between the read and the write; that
  reasoning is wrong and is corrected here.** Not awaiting keeps one task
  from yielding mid-window, but these run on *different* tasks of a
  multi-threaded runtime — three axum handlers and the update worker — so
  two windows can overlap in wall-clock time regardless. The atomic rename
  makes a torn file impossible; it does nothing about a **lost update**: the
  worker appends a block (reads V0, writes V0+block) while a move handler
  reads V0 and writes V0+move, and whichever renames last wins.

  Reach is narrow — the worker only writes this file on a *fresh* install,
  so it takes an operator acting on a different plugin from a second tab
  during one — and every loss is visible and undoable: a lost declaration
  shows as "Installed but not declared" and re-declares, a lost move is
  redone with one arrow. It is left unfixed deliberately, at the end of this
  project, because the fix is a refactor of three request handlers rather
  than a lock added in four places: they build their messages with
  `catalog.read().await` inside the very window that has to be locked, and a
  synchronous mutex cannot be held across that. The shape it should take is
  written out at `plugins::write_atomic`, where the next person to touch this
  file will meet it.
- `run_privileged_unit` (`crates/ritornello-core/src/update/mod.rs`)
  hard-codes the string `"systemctl"` rather than going through
  `SystemInfo::systemctl`, the field that exists precisely so a test could
  substitute `/bin/true` or `/bin/false` for it, the way the power-button
  tests already do. It is no longer untested — a `cfg(test)` red light inside
  that function lets a test answer for the unit, which is how the "the note
  of what was placed is on disk before the process leaves" and "a binary that
  could not be erased says so on the page" tests reach the code they are
  about — but the real `systemctl` is still never run here, and on a real
  failure the page shows systemctl's own generic message ("Job for
  ritornello-update.service failed", or a polkit `Access denied`), while the
  actual cause sits only in the journal.
- `archive::read` decompresses up to 64 MiB synchronously inside an async
  worker task, sharing the tokio runtime with the HTTP handlers. On a Pi 2
  that can hold one runtime thread for several seconds during an update. A
  comment on `run_privileged_unit` already notes that an I/O left without a
  deadline has made a page of this product disappear once before; this is
  the same shape of risk in a different place. Not observed to cause a
  problem on this project's own hardware, not fixed — flagged rather than
  silently carried.

Not something left unverified, but worth recording here for whoever meets
its traces in the history: `plugin_action_refusal`, the scaffold that made
every not-yet-wired plugin gesture refuse honestly instead of silently
doing nothing, was retired once the last gesture (reordering) was wired.
Nothing depends on it any more; it was dismantled on purpose, not
forgotten.
