**Action required** — the core's systemd unit changed
(`ritornello.service`): an update from the web UI replaces the binaries but
never writes a unit, by design. Place the new unit once, either by running
`ritornello-install` from a workstation, or by hand from this release's core
archive:

```sh
sudo tar --no-same-owner -C / -xzf ritornello-core-<version>-<arch>.tar.gz
sudo systemctl daemon-reload && sudo systemctl restart ritornello
```

The one change: `ReadWritePaths=/etc/ritornello -/mnt/ritornello`. Without
it, a network share already mounted when the service starts — after every
boot — is seen read-only by the core and its plugins, whatever it was
mounted with, so archiving a cover onto the NAS fails with "Read-only file
system". A device with no `files` plugin loses nothing by waiting.

This is not a wire break: no contract version moved, and the components can
be installed together or one at a time. The privileged updater and the mount
helper did not change.
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

**One key per source on a wide screen.** From a desktop width up, the home
page shows a bar with one key per source, the active one pressed: changing
source is one click. Pages now use up to 1280 px, and every plugin page is a
card with a title. Narrow screens and the remote are unchanged.

**A progress bar on Oüi FM.** The Oüi FM plugin now gives the position in the
track on air, as Radio France stations already did. The first track after
switching to a station shows its duration but no bar: its start time is
unknown. The bar may run a few seconds ahead of the speakers.

**A cut in a stream leaves a trace.** When a station stops sending data,
the journal (and the System tab's log card) now says when playback stalled
and, once it resumes, for how long — naming the stream. Until now such a cut
left no line at all, and a station's silence could not be told from a fault
of the device.

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
