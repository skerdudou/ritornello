# Installation and operations

## Portability

Nothing in the code is specific to the Raspberry Pi: the remote control
goes through `evdev` (the generic Linux input API, not GPIO), sound through
ALSA/mpv, IPC through Unix sockets — all of which run on any Linux, x86_64
and ARM alike. The Pi 2 is this project's historical reference hardware,
not a technical constraint — the examples below merely illustrate it.

## Installing on a device, without building anything

Everything is installed, updated and removed from your own computer by one
program, `ritornello-install`, which talks to the device over ssh. Every
release on the project's GitHub page carries it, built for five systems.
Take the file for yours from the newest release:

| Your computer | File |
|---|---|
| Windows (64-bit) | `ritornello-install-x86_64-pc-windows-msvc.zip` |
| macOS, Apple Silicon (M1 and later) | `ritornello-install-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `ritornello-install-x86_64-apple-darwin.tar.gz` |
| Linux or WSL, x86_64 | `ritornello-install-x86_64-unknown-linux-musl.tar.gz` |
| Linux, 64-bit ARM | `ritornello-install-aarch64-unknown-linux-musl.tar.gz` |

The Linux builds are linked statically against musl, so they do not
depend on the distribution's C library — not yet measured on an old one. The
release's `SHA256SUMS` lists each of these files, if you want to check the
download.

Then, in a terminal, in the directory where you extracted it:

    ./ritornello-install --host dietpi@192.168.0.57      # macOS, Linux, WSL
    .\ritornello-install.exe --host dietpi@192.168.0.57  # Windows

Without other arguments it asks what to install, and fetches every archive
it needs from GitHub itself, checking each against the release's
`SHA256SUMS`. The [Deploying](#deploying) section below describes the same
program in detail — its screens, its options, removal.

**`--version`.** Without it, the newest final release is installed — or,
while the project has published prereleases only, the newest prerelease.
Name a tag (`--version v0.2.0-beta.2`) to install that release instead;
in a terminal, a screen also offers the choice.

**What the device needs**: an ssh server, an account allowed to use `sudo`,
and a Debian-like system — GNU coreutils, util-linux and systemd; the exact
list is under [Deploying](#deploying), and a busybox-only system is not
supported. Debian, Raspberry Pi OS and DietPi qualify as installed. Nothing
is compiled on the device.

**On your computer**, an `ssh` client:

- **Windows** ships one (the OpenSSH client, an optional feature turned on
  by default since Windows 10 1809). There, ssh cannot keep one connection
  open for several steps, so **without an ssh key it asks for your password
  at each step**. Setting up a key once (`ssh-keygen`, then add the public
  key to `~/.ssh/authorized_keys` on the device) avoids that. Windows
  SmartScreen may warn about an unsigned program on first launch: "More
  info", then "Run anyway".
- **macOS** ships one. The program is not signed by Apple, so the first
  launch is refused until the quarantine mark the browser put on it is
  removed: `xattr -d com.apple.quarantine ./ritornello-install`.
- **Linux** distributions ship one, or it is the `openssh-client` package.

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
the file attached to the release page. The release also carries
`catalogue.json`, a description (kind and one-line summary) of every
installable component, read by the "Add a plugin" dialog —
not a per-architecture archive, so there is only one, whatever the
architecture. Every release also carries `ritornello-install` itself, for
five workstation systems, whether it changed or not (see
[Installing on a device, without building anything](#installing-on-a-device-without-building-anything)).
A single `SHA256SUMS` covers every archive of the release, the installers
included, plus `catalogue.json` and `inventory.json`, whatever the
architecture. A release is installed with
`ritornello-install`, the same program `deploy.sh` runs against a local
directory built like a release (see [Deploying](#deploying) below); an
archive is for putting a specific tagged version onto a device with no
build toolchain at all.

If one leg of the `installer` job fails on a tag (a runner hiccup), the
draft is not created: use "Re-run failed jobs" on that workflow run
rather than pushing the tag again.

**A draft is not yet a release, and pushing the tag is therefore not the
last step.** The workflow deliberately stops at a draft — publishing is the
green light, and the notes are read before anything can install from them —
so the gesture ends with a human publishing it:

```sh
gh release edit vX.Y.Z --draft=false     # or the "Publish release" button
```

Until that happens, **no device can see it at all**: GitHub lists drafts
only to a reader holding push access, and the core polls with no token, so
what the update card reports is "No release published yet" — correctly, and
indistinguishably from a repository that has never released anything. A tag
pushed, a green workflow and 37 attached archives are not evidence a device
can reach any of it. If a device says nothing is published, look first at
whether the release is still a draft.

Four different numbers are at play here, and they answer four different
questions. The **product number** — `vX.Y.Z`, the git tag — names the
release and carries the generation: `0.2.7` is the seventh delivery of the
`0.2` generation. Each shipped component (the core, each plugin) declares
**its own** version, so a fix confined to one plugin does not
renumber everything else and does not make the updater think ten unrelated
components changed too. `PROTOCOL_VERSION`, the wire-compatibility contract
between the core and a plugin (see [plugins.md](plugins.md)), is a third
number, and it moves only on a breaking change to that wire format — not on
every release, and not with every component's own patch bumps. Only the **major** ties
the core, the plugins and the language packs to the product number: an
unchanged component may keep a number from an earlier minor (`0.2.4` inside
product `0.3.0`), and it may never carry a number from a release that does
not exist yet (`0.4.0` inside `0.3.0` is refused). Before 1.0 the major stays
`0`, so nothing forces republishing everything; what keeps the core and the
plugins in step is the shared-crate rule: a change to a crate they all link
must move every component that links it.

The **fourth number** belongs to a root-privileged companion
(`ritornello-files-mount`, see [plugins.md](plugins.md)). It is its own
independent version number, like `PROTOCOL_VERSION`: it answers "did the root
helper change?", is never shown in the UI, is tied to neither the product's
major nor its prerelease suffix, and moves only when the companion
itself changes. That is what lets an unchanged companion keep its number
across every release, so that updating `files` from the web UI never sends
the operator to `ritornello-install` for nothing: only that program can place
the companion.

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

**What republishes, and what does not.** A device decides what to install
by comparing versions and nothing else, so an archive rebuilt under its old
number is fetched by nobody. The script therefore does not republish a
component whose current binary still works with the new core. A component
is published when

1. its own version moved (its code changed, as always);
2. `PROTOCOL_VERSION` changed: a wire break, old binaries can no longer talk
   to the core. Every component that links `ritornello-proto` (the core and
   the plugins; not a companion, which depends on no shared crate, nor a
   language pack, which is data) **must** have moved its version, and the
   script refuses the release, naming those that did not;
3. the product's **major** changed: everything is republished.

A *compatible* change to a shared crate (`ritornello-proto`,
`ritornello-i18n`, `ritornello-plugin-sdk`, `ritornello-updater`) republishes
nothing by itself: the script prints a note on stderr. If the fix must reach
plugins, bump those plugins by hand: that is a delivery choice, not a
compatibility matter. The decision between "break" and "compatible" for the
wire is forced by a test, `crates/ritornello-proto/tests/wire_fingerprint.rs`,
which compares a sample of every wire message with a committed fixture (see
[development.md](development.md#tests)); `PROTOCOL_VERSION` moves at every
break, before and after the first stable release, and a plugin announcing
another number is shown "incompatible" on the System page.

The **first finished release** republishes everything: a finished product
refuses any prerelease component (see below), so every component still on a
beta number moves once. While only prereleases exist there is no finished
release to measure against, so the script behaves as for a first release and
publishes everything.

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
binaries, units and polkit rules — nothing else. A language pack is never
part of a component's archive; see [Installing a language
pack](#installing-a-language-pack) below for that archive's own shape. They
do not
create the `ritornello` system user, install `mpv`/`cd-discid`/`eject`/
`cifs-utils`, create `/var/lib/ritornello` or `/mnt/ritornello`, nor enable
any unit. Extracting them
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

One plugin needs a second archive and a unit enabled by hand the first time
it is installed this way — `files`. Its own archive carries only the plugin
binary; the root mount helper `ritornello-media-mount`, its unit and its
polkit rule ship in the companion archive
`ritornello-files-mount-<version>-<arch>.tar.gz` (the bundle carries both).
Installing `ritornello-plugin-files-*.tar.gz` alone leaves network shares
unable to mount. Extract the companion the same way, then:

    sudo systemctl enable ritornello-media-mount.service

Enabled, **not** started: what it is enabled for is machine boot, where it
reconciles the declared network shares (see [Network
shares](#network-shares)). `deploy.sh` does this for you; an archive cannot,
so a `files` plugin installed from a release and never enabled this way stops
reconciling shares at the next reboot, in silence.

### Installing a language pack

Not by the recipe above. A language pack's archive is **flat** —
`pack.toml` plus one `<module>.toml` per plugin it covers, no leading
path at all — because it is meant to be read by the core's own pack
reader (`crates/ritornello-core/src/langpack/archive.rs`), never
extracted directly onto a device. Running the same
`sudo tar --no-same-owner -C / -xzf ritornello-lang-<language>-<version>.tar.gz`
against it drops `pack.toml` and every module file straight into `/`,
which is not a mistake this project's own tooling ever makes and not one
this recipe should invite either.

The ordinary way to install one is from the config page, where the core
fetches, verifies and writes it itself — no privileged step at all (see
[interface.md](interface.md)). A device with no French installed simply
reads its interface in English until that gesture is made, which is the
correct and unremarkable state of a fresh install, not a fault to chase.

Installed by hand instead — for a device with no network path to
GitHub — a pack extracts into its own directory under
`/etc/ritornello/language-packs/ritornello-lang-<language>/`, replacing
whatever was there, and **never at `/`**:

    sudo mkdir -p /etc/ritornello/language-packs/ritornello-lang-<language>
    sudo tar --no-same-owner -C /etc/ritornello/language-packs/ritornello-lang-<language> \
      -xzf ritornello-lang-<language>-<version>.tar.gz
    sudo chown -R ritornello: /etc/ritornello/language-packs/ritornello-lang-<language>

The core picks it up at its next sweep of that directory (a restart, or
the same resweep an install or removal from the page already triggers);
nothing needs to be declared anywhere else for it.

### Enabling automatic updates (once, by hand)

An update can replace binaries. It can never write a
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
device — the binary, its systemd unit, its polkit rule —
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

Two rules, and neither is a convention that can be bent (they apply to the
core and the plugins; a companion has a number of its own, see above):

1. **The tag equals the product number**, suffix included. So the beta is
   prepared by setting `[workspace.package] version` to `0.2.1-beta.1` and
   tagging `v0.2.1-beta.1`. A workflow step refuses a tag that disagrees.
2. **No component may declare the number the finished release will
   carry.** The device decides by version *equality*, never by order: a
   component shipped as `0.2.1` inside `v0.2.1-beta.1` and shipped again as
   `0.2.1` in the finished `v0.2.1` looks identical to a device that already
   has the beta's bytes, and the tester keeps the older binary for ever,
   silently. So a component the beta delivers carries the beta's own full
   number, suffix included.

   An unchanged component keeps its number across prereleases, with an
   **older** suffix of the same target (`0.2.0-beta.2` inside
   `v0.2.0-beta.3`) or a lower target altogether (`0.2.0-beta.3` inside
   `v0.3.0-beta.1`): never a newer suffix on the same target, never a target
   above the product's, never another major. A finished release refuses any
   suffix, so at the first finished release of a target every component still
   on a beta number moves once. A companion is outside all of this: it moves
   only when it changes itself.

   A component the beta does **not** deliver simply stays where the last
   finished release left it — `0.2.0` while the product prepares
   `0.2.1-beta.1` — and that is the normal shape of a narrow beta: only what
   differs is offered, so there is nothing to be stranded on. A beta may
   therefore ship one component and leave the other ten alone, which is the
   cheapest way to try the machinery.

   Three guards, and they now agree. `version_coherence.rs` refuses a suffix
   that is newer than the product's or on another target number, refuses any
   suffix at all in a finished product (what stops a leftover `-beta.2` from
   riding into a real release), and refuses a component declaring the
   finished number inside a prerelease. `scripts/package-release.sh`
   re-checks the same rules without cargo, since it names the archives; run
   `scripts/package-release.sh --self-test` to see its case table. A
   companion is exempt from all three.

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
`/etc/ritornello/plugins.toml` and every bundled plugin's own data
directory under `/var/lib/ritornello/plugins/` with the defaults (all
bundled plugins declared, two starter stations, MCE remote bindings — the
`deploy/*.example.toml` files), then everything is adjusted from the
browser or by editing those files. An existing configuration is never
overwritten (see [Deploying](#deploying)).

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
  `RITORNELLO_HOST=dietpi@192.168.1.20 ./deploy/deploy.sh` (see
  [Deploying](#deploying)), or
  install `avahi-daemon` to keep using a `<hostname>.local` name.

## Example: generic x86_64 Linux machine

Same packages, minus the Pi-specific steps (no `raspi-config`, the audio
output is picked directly through `/api/audio-output`):

    sudo apt install mpv cd-discid eject cifs-utils

Configuration is provisioned by `ritornello-install` here too (see
[Deploying](#deploying)). `deploy/deploy.sh` works identically:
`TARGET=x86_64-unknown-linux-gnu RITORNELLO_HOST=user@host
./deploy/deploy.sh`. The script still runs `cross` through `build.sh`,
whatever the target.

## Deploying

Every installation goes through one program, `ritornello-install`, which
runs on your workstation and drives the device over ssh. It is what a user
runs against a published release, and it is what a developer runs against
their own checkout: there is no second way in.

    RITORNELLO_HOST=pi@raspberrypi.local ./deploy/deploy.sh

`deploy/deploy.sh` is the development wrapper. It chains `build.sh` (the
npm UI build **then** the cross-compilation — the order guarantees the
embedded SPA is fresh), packages the result with the same
`scripts/package-release.sh` calls the release job makes, writes
`inventory.json` and `SHA256SUMS` beside the archives in `release/install/`,
builds `ritornello-install` **for the workstation** (not for the device),
and hands over: `ritornello-install --from-dir release/install "$@"`. Every
argument you give the script reaches the installer. `TARGET` names the
compilation target (see [Building](#building)) and defaults to the Pi 2's.
`DEPLOY_STOP_BEFORE_INSTALL=1` stops once `release/install/` is complete
and the installer built, without contacting any device. Nothing in the
script places a file on the device: every path, unit and rule comes from
the inventory, generated from `deploy/packaging.toml` like the archives.

Web interface: http://<host>:8080 — logs: `journalctl -u ritornello -f`.

Prerequisites on the workstation: Docker running and `cross` (the script
always goes through `build.sh`, whatever the `TARGET`, and installs `cross`
itself when absent), npm, `python3`, `sha256sum` (GNU coreutils; absent from
stock macOS) and `ssh`.

What the device needs: the installer's script runs on the device under
`sh` and relies on GNU coreutils and util-linux behaviour — `mv -T`,
`rm --one-file-system`, `timeout` and `mountpoint` — plus `tar`,
`systemctl`, `useradd`/`userdel`, and `sudo` when the account is not root.
Debian and DietPi have all of them. A busybox-only system is not
supported.

### The options

    ritornello-install [--host account@host] [--plugins a,b|none]
        [--packs fr,de|none] [--keep] [--remove-all] [--purge-data]
        [--version <tag>] [--from-dir <dir>] [--yes]

- `--host`, or `RITORNELLO_HOST`: the device, as `account@host`.
- `--plugins`, `--packs`: what is wanted, comma-separated; `none` is the
  core alone, respectively no language pack. Whatever is left out is kept
  as it is on the device.
- `--keep`: keep exactly what is installed — a plain update.
- `--remove-all`: remove everything Ritornello placed. `--purge-data` also
  erases the data of what is removed.
- `--version`: the release (a tag) to install; the newest final release by
  default. The wrapper never uses it: it installs its own directory.
- `--from-dir`: install from a directory of local archives instead of a
  release. It must hold `inventory.json`, `SHA256SUMS` and the archives;
  only files `SHA256SUMS` lists are ever read, and each is verified against
  it first.
- `--yes`: do not ask for confirmation.

### What it asks

Without arguments, in a terminal, it asks; with arguments they describe the
state wanted. Every question comes before anything is changed on the
device: the device address and ssh account, what to do with an installation
that is already there, the version, the plugins and the language packs (as
checklists), the data of anything about to be removed, a summary and its
confirmation ("Go ahead?"), and last the sudo password when one is needed.
When an answer is missing and no terminal is there to give it, the run
stops and names it before anything is downloaded or sent.

### Removal and total removal

Removing a plugin (by leaving it out of `--plugins`) removes what the
installer placed for it and, when asked, its data. `--remove-all` removes
everything Ritornello placed — the core, every plugin, the units, the
polkit rules and the packs — and `--purge-data` (or its prompt) also erases
`/var/lib/ritornello` and the `ritornello` account. A removal is planned
from the registry below together with the release's inventory, so it also
removes a file an older version placed and the current one no longer lists.

A total removal also removes the mount root `/mnt/ritornello` itself, after
unmounting and removing every share under it — and only a total removal
does: an install, an update and the removal of just the `files` plugin leave
it in place. The root goes with `rmdir` and nothing stronger, since a
recursive delete of a mounted share would delete the NAS's content. If the
root is still a mount point, or holds anything, the installer stops and
names it; a root already absent is fine. Before anything changes the
installer also refuses a root that is itself a mount point (a share or bind
mount placed right on `/mnt/ritornello`), for the removal of `files` as well,
and on a total removal a file or hidden entry sitting directly in it.

### The registry

`/var/lib/ritornello-install/installed.toml` on the device records what
`ritornello-install` placed, and which release it came from. It is what an
update, a removal and a change of channel are computed from; do not edit it
by hand. If it no longer parses, the installer says so and stops: restore
it from a backup rather than deleting it.

It is also the one file the core reads from `ritornello-install`, for one
field: the version of `files-mount`, the companion that carries the root
mount helper, its unit and its polkit rule. `ritornello-install` alone
installs, updates and removes that companion, always together with
`files`. The web UI updates `files` only when the release offers
`files-mount` at **the same version** as the one recorded here; any other
answer, including no registry or no record of it, sends you to
`ritornello-install`, which updates both (see
[interface.md](interface.md#plugins-table)). A release that changes the
helper therefore says **Action required** in its notes
(`scripts/release-notes-guard.sh` watches `crates/ritornello-files-mount`).

Configuration is provisioned only **when the file is absent** — a first
installation needs no manual copy, and a file that exists is **never
overwritten**, whatever it contains. That is what keeps `stations.toml`
and `input-bindings.toml` (stations added from the browser, learned
bindings) yours. Data written by an older layout is not moved for you:
[Moving data by hand](#moving-data-by-hand) has the old-to-new table.

### Moving data by hand

**There is no automatic migration, and none is planned.** A device
deployed before every plugin moved to its own data directory keeps its
files exactly where they were — `deploy.sh` and the plugins never delete
anything on their own — but nothing reads them from there any more, and a
plugin restarted after an upgrade starts as if it had never run: an empty
station list, no learned bindings, no saved playlist. Move each file by
hand, with the service stopped, before restarting it:

| Old location | New location |
|---|---|
| `/etc/ritornello/stations.toml` | `/var/lib/ritornello/plugins/radio/stations.toml` |
| `/etc/ritornello/input-bindings.toml` | `/var/lib/ritornello/plugins/generic-input/input-bindings.toml` |
| `/etc/ritornello/mpd.toml` | `/var/lib/ritornello/plugins/mpd/mpd.toml` |
| `/etc/ritornello/ouifm-metas.toml`, `radiofrance-metas.toml`, `nrj-metas.toml` | the same file name, under `/var/lib/ritornello/plugins/<plugin-name>/` |
| `/etc/ritornello/media-roots.toml` | `/var/lib/ritornello/plugins/files/media-roots.toml` |
| `/etc/ritornello/media-credentials/` | `/var/lib/ritornello/plugins/files/credentials/` |
| `/var/lib/ritornello/plugin-radio.json`, `plugin-cd.json`, `plugin-musicbrainz.json` | `/var/lib/ritornello/plugins/<plugin-name>/state.json` |
| `/var/lib/ritornello/plugin-files.json` | `/var/lib/ritornello/plugins/files/state.json` |
| `/var/lib/ritornello/plugin-files.m3u` | `/var/lib/ritornello/plugins/files/playlist.m3u` |
| `/var/lib/ritornello/playlists/` | `/var/lib/ritornello/plugins/files/playlists/` |

**Ownership is yours to fix after a hand move.** Neither `deploy.sh` nor
`ritornello-install` runs a recursive `chown` over existing data: the
installer changes the owner only of directories it creates itself, and of
a plugin's data directory when it places an initial configuration there.
A file or directory you moved in as root stays root's, and a plugin cannot
write into what it does not own. So, once everything is moved, you MUST
run

    sudo chown -R ritornello:ritornello /var/lib/ritornello/plugins

and keep `/var/lib/ritornello/plugins/files/credentials/` at mode `0700`
with the files inside it at `0600`.

### The operator's own locales layer is gone

`/etc/ritornello/locales` is no longer read by anything (owner's decision,
2026-09-23, no backward compatibility): a device deployed before this
delivery may delete it by hand.

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
device access, which comes through groups. `ritornello-install` creates a
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
user: the core itself persists `plugins.toml` there through atomic writes
(`.tmp` then rename) — enabling, moving or removing a plugin from the admin
UI rewrites it — which requires write access to the directory itself.
`/var/lib/ritornello` is owned the same way: every plugin keeps its own data
directory there (`RITORNELLO_PLUGIN_DATA_ROOT`, `/var/lib/ritornello/plugins/
<name>/` by default — see [plugins.md](plugins.md#where-a-plugin-keeps-its-data)),
created and rewritten by the service.

Installing by hand instead of through `ritornello-install`? The installer
creates the user and creates the directories it needs owned by it; it never
changes the owner of a directory that already exists. By hand, these are
the two commands:

    sudo useradd --system --home-dir /var/lib/ritornello --no-create-home \
      --shell /usr/sbin/nologin ritornello
    sudo chown -R ritornello: /etc/ritornello

An installation deployed before this change ran as root. The next
`ritornello-install` creates the user and replaces the unit, but it does
**not** change the owner of a tree that already exists: run
`sudo chown -R ritornello:ritornello /etc/ritornello /var/lib/ritornello`
yourself, or the service cannot write there.

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

`ritornello-install` installs `deploy/50-ritornello-power.rules` into
`/etc/polkit-1/rules.d/`. It grants the `ritornello` user the six logind
actions involved — power-off and reboot, each in its plain,
`-multiple-sessions` and `-ignore-inhibit` form. All six, because logind
checks the plain action only when nothing else is going on: it switches to
`-multiple-sessions` as soon as another session exists (an open SSH
connection is enough, which is the usual situation while testing) and to
`-ignore-inhibit` when an inhibitor is held — which also means a confirmed
shutdown overrides shutdown inhibitors and will not wait for an
in-progress `apt`/`dpkg` run to finish.

polkit itself is not installed by `ritornello-install` — it installs no
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
`/mnt/ritornello`, and enables the mount unit so shares come back
after a reboot. `/var/lib/ritornello/plugins/files/credentials` (mode
`0700`, owned by the service) is not the script's doing: the plugin creates
it itself, on first use. On a device already in service, the `files` entry
of `plugins.toml` is appended by the same run — see [plugins.md](plugins.md).

**Declaring a share** happens in the browser, at
`http://<host>:8080/plugins/files/`. Give the server address, connect, and
the wizard lists the shares it exposes; pick one, walk down to the folder
you want, and confirm. Nothing is mounted until you do.

You are not asked to name anything. The internal name is derived from the
share and de-duplicated, because it becomes both a directory name and a
credentials filename — deriving it guarantees a valid one, where typing it
allowed a refusal with no way to see why.

Confirming writes `/var/lib/ritornello/plugins/files/media-roots.toml` and
`/var/lib/ritornello/plugins/files/credentials/<name>.cred`, then asks
systemd to run the mount unit on its own. The mount point is not yours to
pick: it is always `/mnt/ritornello/<name>`.
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

- `ritornello-install` placing the updater's binary, its two units and its polkit
  rule — the block exists and a test holds it to `packaging.toml`, but no
  deployment has run since it was written, so the first one is also its
  first trial;
- `systemctl start ritornello-update.service` running with the actual
  polkit rule in place;
- a running binary being replaced on disk while its own process is live;
- systemd restarting `ritornello.service` after an update, rather than a
  test process exiting on its own;
- the rollback firing;
- updating `files` from the web UI on the Pi, while its companion
  `files-mount` keeps its version. The comparison of the release's
  companion version with the one `ritornello-install` recorded in
  `installed.toml` has only run against a test release and a test
  registry, and no device has yet read a registry an actual installer run
  wrote;
- a release carrying a companion, since the two prereleases that shipped
  `ritornello-files-mount-<version>-<arch>.tar.gz` (beta.2 and beta.3): no
  device has yet had the core recognise one in GitHub's own listing, nor
  `ritornello-install` place one fetched from a release. The companion has
  since moved to its own numbering (`1.0.0`), which no release has carried
  yet;
- `ritornello-install` removing a plugin on the Pi (unchecking it) and then
  placing it again (checking it back). Each direction is covered by tests
  that run the generated script for real, but the round trip has not been
  run on the device;
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
  archives named after different numbers, a catalogue read from one
  page of a hundred releases — has been exercised by an actual device
  fetching an actual release.

**Repeat-one: partly measured on a bench, never on the device.** Two things
are unverified:

- Repeat-one on a CD: the seek back to the playing track's start lets an
  instant of the next track through; its length has never been measured (no
  CD drive was available);
- mpv's `loop-file`, which the core arms for the files source under
  repeat-one, was measured on the development bench (mpv 0.37.0 under WSL),
  never on the Pi: a file in a playlist loops without being reloaded, and an
  unreadable entry is tried once, after which mpv moves on to the next
  entry. The mpv version the device runs may differ.

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

**The language-packs chantier has not been verified on real hardware,
physical display included.** Sixteen tasks rebuilt how text is resolved —
a plugin now emits only `(key, params)`, the core holds every language
layer and resolves through chosen language → fallback → English → the key
itself — and the whole rebuild is covered by the Rust and web test suites,
plus a parity test proving the core's own resolver and the browser's agree
on the same catalog. None of that has been watched happening on the
Raspberry Pi this project targets. In particular, the twenty-column console
display (`ritornello-plugin-console`) is exactly what an earlier defect
report on this project described as broken, and nobody has yet watched its
`no disc` become `pas de disque` on the real screen after a language
change — until that observation is made, this line stays, per this
project's own rule against shrinking this section on faith.

**No language pack has ever been installed on the device.** Building one,
publishing it, fetching it, verifying its digest, refusing a malformed one,
writing it, resweeping the registry and removing it are all covered by
tests that fake the network and use a temporary directory; none has run on
a Pi, and the pack job of the release workflow has never run at all — like
the rest of that workflow.

**Sources — third-party repositories — have never met a real third party.**
The list of sources, the fresh install of a third-party plugin, the
per-language install of third-party packs and the fourth automatic policy are
covered by tests that fake the network and by the web suites. Specifically:

- no third-party repository exists. Nothing of this has run against a real
  stranger's release, and nothing has run on the Pi;
- the parallel sweep's 20-second deadline is proven under a paused clock, plus
  one test over local sockets on the real clock with a short injected
  deadline; it has not been observed against slow hosts on a real network;
- a real third-party `catalogue.json` has never been read, so the install
  dialog's descriptions of a stranger's plugin have only met fixtures;
- **while only prereleases are published, a device on the stable channel is
  offered no fresh third-party plugin at all** — and the same holds while
  nothing is published. Whether a name is ours cannot be judged without our
  release list (`Worker::settle_without_release`), and a name that merely
  looks free would let a stranger be the only owner of one of ours; the core
  chooses to offer nothing fresh and to declare no conflict. This is deliberate
  and is today's situation for a stable-channel device, since no stable release
  has been published: a device that ticked "Offer prereleases" sees our
  release list and so sees the offers. Updates to third-party plugins already
  installed, and third-party language packs, are still offered in that state;
- a third-party language pack the pack reader refused is marked "do not retry"
  in memory only (`remember_manual_step` on the row), so the automatic policy
  does not fetch the same refused archive every night; the mark is forgotten
  when the core restarts, which costs one extra download per restart;
- `ritornello-install` leaves `ritornello-xlang-*` packs to the core: its
  per-pack step neither installs nor removes them, and it removes them only
  with the rest of `/etc/ritornello` on `--remove-all`. That is unit-tested
  against a generated plan, never run on a device;
- a core older than the one that introduced the fourth policy (reached by a
  rollback, or by a channel switch that installs an older core) discards the
  whole of `state.json` when it meets that value (see [the update
  policy](interface.md#automatic-update-policy)); this has been reasoned from
  the old loader and never tried by rolling a device back.

**The German, Spanish and Italian packs have not been reviewed by a native
speaker.** They were translated from the English catalogs, with the French
pack as a reference for meaning, and the only thing checked mechanically is
what the parity tests check: the same keys as English, the same `{named}`
parameters in every value. Wording, register and terminology (what a
preset, a source or a plugin is called) are unverified, and so is whether
the short status words still fit the twenty-column console display. The
first native reader to open one of these interfaces is its first review.

**The removal of the operator's own locales layer has been exercised on
exactly one device, by hand, and nowhere else.** The owner deleted
`/etc/ritornello/locales` on the one real Raspberry Pi this project has
(verified); the code and script changes that make that path unreadable
and unwritable everywhere else — the narrowed archive rule
(`update::archive::ETC_PREFIXES`), the trimmed `deploy.sh`, a fresh
deployment never creating that directory at all — have run only in this
repository's own test suite, never against a real deployment from scratch.

**The privileged-plugin refusal has never been seen on the Pi.** Refusing
to install or uninstall `files` from the web UI — the route returning
403 before it stops the plugin, the archive check refusing its download,
the table and the dialogs showing the sentence instead of a button — is
covered entirely by unit and component tests built against fakes; nobody
has clicked "Uninstall" for the files plugin on the real device to watch
it stay running and declared, nor tried installing it from a release
archive to watch the refusal actually name `ritornello-install`. That
program now exists in this repository (see the next paragraph), but the
hand-off to it has not been seen on a device either.

**`ritornello-install` has never completed a cycle on a device.** It is
built, unit-tested and driven against fakes. The full `deploy/deploy.sh`
chain (`npm ci`, the builds, the `cross` build for armv7, packaging, the
inventory and `SHA256SUMS`) has been run in WSL up to the hand-over and no
further: the install step itself has not run. No install, update,
removal or total removal has run on a real Raspberry Pi until the end of
the delivery's own device trial, so nothing in [Deploying](#deploying) is
proven on hardware. In particular:

- it has never been launched from Windows or macOS, only from a Linux
  workstation (WSL). Since the `installer` job of `ci.yml`, it is
  **compiled and unit-tested** on Windows and on Apple Silicon macOS on
  every change (the Intel macOS build is compiled only; the tests that
  replay the device script for real run on Linux alone, since that script
  needs the GNU tools of the device, not the workstation's), but never run
  against a device from either; in particular the Windows path, where ssh
  runs one connection per step and the terminal screens draw on the
  Windows console, has never been seen working end to end;
- no release has yet carried the five installer archives: the publish
  job's step that adds them after its component filter, and the
  `SHA256SUMS` lines that cover them, run only on a tag, and no tag has
  been pushed since they were written — the next one is their first trial.
  The instructions in
  [Installing on a device, without building anything](#installing-on-a-device-without-building-anything)
  describe files that do not exist until then;
- `sudo -S` (reading the password from its standard input) and `sudo -k`
  (dropping the cached credential) have never been measured on a device,
  whatever their documented behaviour;
- the ssh `ControlMaster` socket path it builds has never been measured on
  macOS, where the length limit on Unix socket paths is tighter;
- `inventory.json` has never been produced by a real publish job: the
  installer reads one generated locally by `scripts/install-inventory.py`,
  and the release workflow's own run of that step, and the `SHA256SUMS`
  line covering it, have not yet happened;
- the registry, `/var/lib/ritornello-install/installed.toml`, has never
  been written or read on a real device.

**The move to one data directory per plugin has never run on a device.**
Every plugin now writes only inside its own
`/var/lib/ritornello/plugins/<name>/`, in place of the mix of fixed
`/etc/ritornello/*.toml` files and `/var/lib/ritornello/plugin-*.json`
files it used before — covered by the Rust test suite (including the
static guard that scans every plugin crate's own source for a stray path
outside that scheme) and by the end-to-end harness, never by a real
upgrade. In particular: `deploy.sh` creating each plugin's directory
(`mkdir -p`) and copying its default configuration into it **only when
absent**, an update archive's `initial-config/` entries landing in the
plugin's own directory rather than the old fixed locations, and the core
actually creating `/var/lib/ritornello/plugins/<name>` before a plugin's
first launch in service — none of it has been watched happening on a
Raspberry Pi, on a fresh install or an upgrade from an older release. See
[Moving data by hand](#moving-data-by-hand) above for what an operator
upgrading an already-deployed device would need to do themselves: there is
no automatic migration, and none is planned.

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
