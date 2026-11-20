**Nothing to do** — replace the binaries. A device on 0.2.0, 0.3.0-beta.1 or
0.3.0-beta.2 updates from the page once "Offer prereleases" is ticked.
<!-- Line 1 is checked by scripts/release-notes-guard.sh: it must open with
     "**Action required** — <what to do by hand>" when a systemd unit, a polkit
     rule, the updater or the mount helper changed since the previous release,
     or when the bootstrap PROTOCOL_VERSION or a contract major moved (wire
     break: install the core and every plugin together). -->

**Ritornello 0.3.0-beta.4 — quieter stutter on the lock chime, covers that come back, tags on web downloads, phone fixes.**

**Before rolling back generic-input to 0.2.0**, remove the lock key's binding:
the old binary would otherwise ignore every key of the remote. A rollback of
the core to 0.2.0 forgets the alarms: note them first.

## What's new since 0.3.0-beta.3

- **Lock chime** — the chime is now pre-rendered and built into the core, instead of being synthesised by ffmpeg while the music plays, which was the main suspect for the stutter. It is also louder (about +4 dB).
- **Covers that come back** — a cover read from a slow network share is retried four times over about twenty seconds, instead of twice over five.
- **Enlarged cover** — fills the screen, without the 16 px margin around it.
- **Track list** — the pages follow your finger while you swipe, and the scrollbar no longer sits against the right edge.
- **Web streaming downloads** — each track is tagged with its title, its author (the channel, or the artist when the site gives one), the video's description and its YouTube address. Album and track number are written only when the site gives them. Other sites are not covered yet.
- **Phone** — the web-streaming download dialog, the files browse tab (buttons with icons) and the generic-input buttons no longer overflow or take the whole width.

Not yet verified on the Pi: whether the stutter is gone (it was measured
as slower than real time before this change, not after), how the chime
sounds at its new level, the tags on a real download, and the swipe and
phone layouts on a real phone.

## What's new since 0.2.0

- **Equalizer** — 10 bands on the Configuration page, for every source; seven presets plus your own; automatic anti-clipping compensation.
- **Remote lock & chime** — hold a key bound to "Lock (long press)" 2 s, or tap the padlock in the Player card: the remote ignores every other key.
- The displays show LOCKED / UNLOCKED; a soft chime (five to choose from, or none, with a preview) plays on each change.
- **MCE remote preset** — Clear is the lock key; red and green toggle random and repeat; Video, Audio and TV pick the cd, files and radio sources.
- **On a phone** — every page fits a 412 px screen; reorder lists by finger with the ⠿ handle.
- Presets as a scrolling list or swiped pages; pinch, pan and double tap on the enlarged cover; volume in steps of 5, heard while dragging.
- **Sleep timer & alarms** — a sleep timer beside the standby key (immediate, fade, end of track), with a typed duration and +15 min.
- Phone-style alarms on the config page: time, days, source, volume, optional "turn off after"; the console shows the timer and the next alarm.
- **Web streaming** — links are added from a dialog and progress in the list; every change is saved at once (no Save button).
- Keep part of a single video (cut times to the millisecond, needs ffmpeg).
- The square album art of an album track; a playlist can take the folder's `cover.jpg` alone.
- **Files** — a track's embedded cover, tags and lyrics now follow every track change (the new cover was sometimes never shown).
- **Other** — an **auto** theme that follows the system; the empty Length column of files is gone.
- The journal names who asked for an update; French, German, Spanish and Italian cover the new texts.

Never tried on the Pi: the equalizer, the remote lock and its chime, the sleep
timer and alarms, the phone pages on a real phone, the web streaming add
dialog, the embedded-cover fix (see docs/installation.md, "What has not been
verified").

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
