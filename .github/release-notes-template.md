**Action required** — wire break: install the core and every plugin
together. The bootstrap `PROTOCOL_VERSION` moved from 1 to 2, so every
component of this release carries a new number and none of the previous
ones can talk to the new core (nor the new plugins to an old core).

- **Before installing**, set *Automatic checks* to *Off* or
  *Check only* (Settings → Updates). The core you are running now
  predates grouped updates: left on *Check and install*, it would install
  this release one component at a time during the night, and a failed
  start of the new core would be rolled back alone — leaving every new
  plugin refused by the old core, and the device silent.
- Then install everything in one go, while you watch: the update dialog
  with every component checked, or `ritornello-install` from a
  workstation. From this release on, the core installs a break as one
  group and rolls it back as one.
- A rollback of the core leaves every plugin refused until the core is
  reinstalled.

The privileged updater's crate changed only in its tests: nothing to
reinstall for it. No unit, polkit rule or mount helper changed.
<!-- The opening block above is this release's answer to "is there anything
     to do by hand?", and it is rewritten at every release rather than left
     to rot. When the next one needs nothing, the whole block above becomes
     one line:

     **Nothing to do** — replace the binaries.

     and when it does need something, it stays as it is here:

     **Action required** — <what to do by hand>.

     In 0.x semver the minor carries breaks, so the digit alone cannot say
     this. The sentence can.

     Action required when this release changes any of:

     - a systemd unit (`deploy/*.service`)
     - a polkit rule (`deploy/*.rules`)
     - `ritornello-media-mount`, the helper root runs for network shares
       (the `files-mount` companion archive, which only ritornello-install
       places)

     The updater cannot write any of those, by design. Say so here, with the
     command, or the device will update its binaries and silently keep the
     old unit.

     A release that moves the bootstrap `PROTOCOL_VERSION` (or a contract's
     major) is a wire break, and needs this line too:

     **Action required** — wire break: install the core and every plugin
     together. A rollback of the core leaves every plugin refused until the
     core is reinstalled. -->


## Behaviour changes

**Where is this playing from?** The `(?)` next to the track on the player
page now opens with a *Source* row: the stream URL of the station, or the
address of the file — `smb://host/share/…` for a file on the NAS (never the
user name), its path on the device otherwise. The CD shows none. The `(?)`
now appears even when nothing else is known about what plays, such as a
station that announces no title.

**A disc can start playing when it is inserted.** The CD plugin's page has a
new setting, *When a disc is inserted*: nothing (the default), play if the CD is
the source being listened to, switch to the CD and play, or also leave
standby to do so.

**Faster tracks from a folder with a cover.** mpv no longer opens the
`cover.jpg` beside every track itself (4.5 s to start a track in such a
folder on the Pi, against 1.8 s elsewhere). A cover archived onto the NAS is
now credited to the folder for the rest of the album, and archiving a large
cover no longer marks a healthy share as unresponsive.

**Each wire contract has its own version.** A plugin now announces one
version per contract it speaks (source, display, input, metadata, admin).
The System page shows a plugin whose contract major differs from the core's
as *incompatible*, naming the contract, and one whose minor is newer as
*limited*: it runs, with some features inactive. A future break of one
contract will only require the plugins that speak it to move.

**A breaking core is installed with its dependents.** When an update changes
a contract's major, the core and the plugins that depend on it are installed
in one step and rolled back together if the new core does not start. The
night-time update never installs a break; it waits for you. This applies to
updates installed *by* this core — hence the action above for this one.

## Install

Common case — the core and every plugin:

```sh
sha256sum -c SHA256SUMS --ignore-missing
sudo tar --no-same-owner -C / -xzf ritornello-core-<version>-<arch>.tar.gz
sudo tar --no-same-owner -C / -xzf ritornello-plugins-<version>-<arch>.tar.gz
sudo chown -R ritornello: /etc/ritornello
sudo systemctl daemon-reload && sudo systemctl restart ritornello
```

`--no-same-owner` is belt and braces, not a workaround: the archives are
built with every entry owned by `root`, and the packaging script refuses to
produce one that is not. But extracting as root restores whatever ownership
an archive carries, so the flag makes the recipe safe even for an archive
built before that rule existed — a polkit rules file (JavaScript `polkitd`
runs as root) owned by a non-root uid is a local privilege escalation.

Each archive holds the tree as it will exist on the device, and holds no
configuration you may have edited: `stations.toml`, `input-bindings.toml` and
`plugins.toml` are never in it. Example files and the `plugins.toml` block of
each plugin travel beside the tree, to be copied by hand.

Replacing a single plugin is the same two commands with that plugin's archive.

The `files` plugin needs its companion, `ritornello-files-mount`, which
carries the root mount helper, its unit and its polkit rule: the plugin's own
archive does not. After installing `ritornello-plugin-files` together with
`ritornello-files-mount` (or the bundle, which carries both) for the first
time:

```sh
sudo systemctl enable ritornello-media-mount.service
```

Enabled, **not** started: what it is enabled for is machine boot, where it
reconciles the declared network shares. Skip it and declared shares silently
stop being mounted after a reboot.

A language pack (`ritornello-lang-<language>-<version>.tar.gz`) is not part
of the recipe above, and not part of any component's archive either. It is
**flat** — `pack.toml` plus one file per plugin it covers, no leading
path — so the `tar -C /` above must never be run against it: doing so drops
those files straight into `/`. Install one from the config page, where the
core fetches, verifies and writes it itself with no privileged step at all;
or, by hand, extract it into its own directory:

```sh
sudo mkdir -p /etc/ritornello/language-packs/ritornello-lang-<language>
sudo tar --no-same-owner -C /etc/ritornello/language-packs/ritornello-lang-<language> \
  -xzf ritornello-lang-<language>-<version>.tar.gz
sudo chown -R ritornello: /etc/ritornello/language-packs/ritornello-lang-<language>
```

## What this release carries

This release carries the components whose own version moved since the
previous one. When the bootstrap `PROTOCOL_VERSION` or the product's major changed, every
component moved; when a wire contract's major changed, the core and every
plugin that speaks it moved (the release script refuses a break that left one
of them on its old number). A component missing from the assets below is
unchanged, not removed — do not read its absence as a regression.

**If a shared crate changed** (`ritornello-proto`, `ritornello-i18n`,
`ritornello-plugin-sdk`, `ritornello-updater`) compatibly, nothing was
republished for it: devices install on version inequality alone, so a
component only reaches them under a new number. If the fix must reach a
plugin, that plugin's version must have been bumped by hand in the same
commit.

The version in an attached file's name is that **component's** own version,
not this release's tag: `ritornello-plugin-radio-0.2.1-<arch>.tar.gz` says
nothing about the tag it ships under. Only `ritornello-plugins-<version>`,
the bundle of every plugin, is named after this release's own tag.

**Never delete a published release or its attached files.** The appliance
goes back to fetch, from there, the components that have not changed since.

## Architectures

- `armv7` — Raspberry Pi 2 and similar, the reference hardware.
- `x86_64` — PC/server; the target this project's tests and end-to-end
  journeys run on.
- `arm64` — Pi 3/4/5 class. **Cross-compiled but never started on hardware**,
  for lack of a device to try it on.
