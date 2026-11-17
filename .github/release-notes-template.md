**Nothing to do** — replace the binaries. A device on 0.2.0, 0.3.0-beta.1 or
0.3.0-beta.2 updates from the page once "Offer prereleases" is ticked.
<!-- Line 1 is checked by scripts/release-notes-guard.sh: it must open with
     "**Action required** — <what to do by hand>" when a systemd unit, a polkit
     rule, the updater or the mount helper changed since the previous release,
     or when the bootstrap PROTOCOL_VERSION or a contract major moved (wire
     break: install the core and every plugin together). -->

**Ritornello 0.3.0-beta.3 — equalizer, remote lock, phone-friendly page.**

**One click for a switched-off plugin.** A plugin already switched off when
this release lands reads `? → 0.3.0-beta.3` and is not updated at night: one
click on **Install** fixes it for good.

**Before rolling back generic-input to 0.2.0**, remove the lock key's binding:
the old binary would otherwise ignore every key of the remote. A rollback of
the core to 0.2.0 forgets the alarms: note them first.

## What's new since 0.2.0

- **Equalizer** — 10 bands on the Configuration page, for every source; seven presets plus your own; automatic anti-clipping compensation.
- **Remote lock & chime** — hold a key bound to "Lock (long press)" 2 s, or tap the padlock in the Player card: the remote ignores every other key (no key is bound by default).
- The displays show LOCKED / UNLOCKED; a soft chime (five to choose from, or none, with a preview) plays on each change.
- **On a phone** — every page fits a 412 px screen; reorder lists by finger with the ⠿ handle.
- Presets as a scrolling list or swiped pages; pinch, pan and double tap on the enlarged cover; volume in steps of 5, heard while dragging.
- **Sleep timer & alarms** — a sleep timer beside the standby key (immediate, fade, end of track), with a typed duration and +15 min.
- Phone-style alarms on the config page: time, days, source, volume, optional "turn off after"; the console shows the timer and the next alarm.
- **Web streaming** — keep part of a single video (cut times to the millisecond, needs ffmpeg).
- The square album art of an album track; a playlist can take the folder's `cover.jpg` alone.
- **Other** — an **auto** theme that follows the system; the empty Length column of files is gone.
- The journal names who asked for an update; French, German, Spanish and Italian cover the new texts.

Never tried on the Pi: the equalizer, the remote lock and its chime, the sleep
timer and alarms, the phone pages on a real phone (see docs/installation.md,
"What has not been verified").

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

**Never delete a release that still holds some component's current
archive.** The appliance goes back to fetch, from there, the components that
have not changed since.

## Architectures

- `armv7` — Raspberry Pi 2 and similar, the reference hardware.
- `x86_64` — PC/server; the target this project's tests and end-to-end
  journeys run on.
- `arm64` — Pi 3/4/5 class. **Cross-compiled but never started on hardware**,
  for lack of a device to try it on.
