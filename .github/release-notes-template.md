**Nothing to do** — replace the binaries. A device on 0.2.0, 0.3.0-beta.1 or
0.3.0-beta.2 updates from the page once "Offer prereleases" is ticked.

**Ritornello 0.3.0-beta.3 — equalizer, remote lock, phone-friendly page.**
The third beta of the 0.3 generation. It is a prerelease: only a device that
ticked "Offer prereleases" is offered it.

No systemd unit, polkit rule, privileged updater or mount helper changed
since 0.3.0-beta.2: the root mount helper `files-mount` 1.0.3, the privileged
updater 1.0.0 and the installer 0.5.0 keep their numbers. This is not a wire
break: two contracts moved by a compatible minor (input 1.2, display 1.4),
and the bootstrap `PROTOCOL_VERSION` did not move.

**One click for a switched-off plugin.** A plugin that is already switched
off when this release lands still has a binary without the new version block:
its row reads `? → 0.3.0-beta.3` and the night run leaves it alone. One click
on **Install** fixes it for good.
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

## Behaviour changes since 0.3.0-beta.2

**Equalizer.**
- 10 bands (31 Hz – 16 kHz, ±12 dB) on the Configuration page, applied to
  every source; seven built-in presets plus up to 20 of your own.
- Automatic compensation (on by default) keeps boosts from clipping; changes
  apply without clicks, even while a station is connecting.

**Remote lock (child lock).**
- Hold a key bound to the new **"Lock (long press)"** action for 2 s, or tap
  the padlock in the Player card: the physical remote ignores every other key
  until unlocked the same way. The page and the mpd apps keep working.
- No key is bound by default: bind one on the generic-input page.
- The displays show LOCKED / UNLOCKED, a refused key shows LOCKED, and a soft
  chime plays (choice of five, or none, under Settings → Player, with a
  preview on the device), also in standby and while paused.
- The lock survives the restart of an update, and only that one.

**On a phone.**
- Every page fits a 412 px screen; generic-input's buttons become icons.
- Reordering by finger with the ⠿ handle in the four reorderable lists.
- Presets: a scrolling list mode, and a swipe turns the pages.
- Enlarged cover: pinch, pan, double tap.
- Volume in steps of 5, heard while dragging.

**Other changes.**
- Theme: a third mode, **auto**, follows each browser's system setting.
- files: the always-empty Length column is gone.
- web-streaming: the square album art of an album track instead of the 16:9
  frame; a playlist can take the folder's `cover.jpg` alone; cut times accept
  milliseconds (`1:30,25`).
- Updates: a switched-off plugin's version is now read from its binary, so
  its row and the night run judge it like the others (see the note above);
  the journal names the address that asked for a check or an install.

French, German, Spanish and Italian cover the new texts.

**Before rolling back generic-input to 0.2.0:** remove the lock binding
first. The old binary does not know the action and then ignores every
binding of the remote.

This release carries, each as `0.3.0-beta.3`: `ritornello-core`, all twelve
plugins and the language packs fr, de, es, it. `files-mount` stays on its
1.0.3 archive, in the v0.2.0 release.

Never run on the Pi: the equalizer on a real output, the remote lock with the
real receiver and its chime, the phone pages on a real phone. The full list
is in docs/installation.md, "What has not been verified".

## Since 0.3.0-beta.1

**Sleep timer: any duration.** The menu has a field for the duration, in
whole minutes from 1 to 600, started with **Start** or Enter. The keys 15,
30, 45, 60 and 90 min no longer start a timer by themselves: they fill the
field, so "15" can be corrected to "20" before starting. The field is empty
at each opening, and anything outside the bounds keeps **Start** greyed.

French, German, Spanish and Italian cover the new texts.

## Since 0.2.0, for a device coming from it

**Sleep timer.** A key beside the standby key, in the Player card's header.
Its menu starts a countdown, then puts the device in standby exactly as the
Power key does. Three ways to end:

- **Immediate** — standby at the deadline;
- **Fade** — the volume goes down to zero over 10 s, then standby; the next
  listening starts at the level it left, never at zero;
- **End of track** — keeps playing until the current track ends (5 minutes
  at most), then fades.

While a timer runs the key shows the time left, and the menu offers
**+15 min** and **Cancel**. The menu remembers the last mode chosen. The key
is disabled in standby. Scripts can use the same three commands:
`SetSleepTimer`, `ExtendSleepTimer`, `CancelSleepTimer`.

**Alarms.** A new card on the config page, phone-style: one line per alarm,
with a switch to turn it on or off. Each alarm has a time, its days (at least
one; Monday first), what it plays (resume, or a named source), a volume, and
optionally "turn off after" N minutes, which arms an ordinary sleep timer when
it rings ("wake me at 7, turn off at 8"). An alarm takes the device out of
standby and starts playing; it does nothing if something already plays. An
alarm missed during an update's restart is caught up.

**Console.** The text console shows the time left on a running sleep timer,
and the next alarm under the standby clock.

**Web streaming: keep part of a single video.** The download dialog accepts a
start and an end (`h:mm:ss`, `m:ss` or seconds) for a single video; the plugin
keeps that part without re-encoding. It needs ffmpeg on the device: without
it the fields are disabled.

**Languages.** French, German, Spanish and Italian cover the new texts.

**Before trying it: a rollback to a core without alarms forgets them.** The
0.2.0 core does not know the alarms and rewrites its state without them on
its first write: coming back to this beta does not bring them back. Note your
alarms before rolling back.

The sleep timer and the alarms **have never run on the Pi** — the fade's
sound on a real output, the end-of-track detection on stations whose title
never changes, the alarm catch-up after a real update or a rollback, and
alarms before the clock is set by the network are all tested, not observed.
The full list is in docs/installation.md, "What has not been verified".

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
