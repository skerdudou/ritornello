# The interface

## Web remote and command API

The home page (`http://<host>:8080/`) embeds a remote control: the 12
commands of the protocol (presets 1-9 plus `+10`, next/previous, volume,
mute, play/pause, stop, eject, source switch, standby).

`Next`/`Prev` are interpreted by the active source: preset for the radio,
track for the CD player — these are not two distinct command pairs, only a
semantic that varies with the source. A binding that still references
`NextTrack` or `PrevTrack` (the old name) is no longer valid: it must be
rewritten as `Next`/`Prev`.

The remote goes through `POST /api/command`, whose body is exactly a
protocol command — the same channel the Input plugins feed, so no business
logic is duplicated:

    curl -X POST http://<host>:8080/api/command \
      -H 'content-type: application/json' -d '{"cmd":"VolumeUp"}'
    curl -X POST http://<host>:8080/api/command \
      -H 'content-type: application/json' -d '{"cmd":"Select","arg":3}'

Handy for driving the device without a remote (from a phone on the local
network, or over SSH while debugging).

The **Player** card above the remote (active source, volume, mute,
standby, and the current track with where the information came from) is
fed by a pushed stream from `GET /api/player` (SSE) — nothing is polled,
and the state follows the infrared remote as well as other browser tabs.

On the preset grid, the key matching **what is playing** is highlighted:
the preset for the radio, the track for the CD. The active source is what
declares it (the `preset` field of its frames, see the protocol) — the
core never interprets what `Select(n)` was supposed to mean — and it goes
out as soon as nothing is playing anymore.

The grid is not hardcoded to nine keys: the active source also declares
**how many** presets it has (`preset_count` in its frames) — stations for
the radio, tracks for the CD — and the web UI shows only the numbers that
exist. Absent means the source says nothing on the subject, so the grid
falls back to the historical 1-9 layout rather than being disarmed by a
source that has not been updated; `Some(0)` is a distinct, meaningful
answer ("nothing to number", an empty CD tray). The remembered count is
forgotten on a source change and on standby (the newly active source
re-declares it on activate/wake) but **not** on stop — a stopped radio
still has its stations.

Past the ninth preset, bare digits cannot reach further: the **`+10`**
key — the web grid's own button, or the corresponding key on the physical
remote once bound — accumulates a tens offset held by the **core**,
cumulatively (each press adds 10), wrapping back to 0 once it passes the
last useful decade (`(count / 10) * 10`, so a count of 20 still lets
`+10 +10` then `0` reach preset 20; with no known count, the offset
saturates instead of wrapping). It is shown as `+NN` through the same
overlay slot and the same 2 s deadline as the volume/mute overlay, so a
further `+10` within that window extends it rather than starting a new
one. The next digit (`Select`) consumes the pending offset — effective
number = offset + digit — and clears the overlay; any other command
abandons a pending offset outright, since pressing, say, a volume key
mid-sequence is a change of mind, not a step of it. Key **`0`** is legal
input for exactly this: alone, with no offset pending, `Select(0)`
selects nothing (there is no preset 0).

The web grid mirrors the same decade window **locally**: its own `+10`
button (shown once the count exceeds nine) shifts the visible numbers to
the next ten, wraps at the same boundary as the core, and auto-returns to
the base 1-9 window after 2 s of inactivity or as soon as a preset is
picked — the browser always sends the absolute number to `Select`, `+10`
itself never leaves it.

**Volume +/- respond to holding**, not just clicking: pointer-down sends
one step immediately, then — after the initial delay set on the config
page's volume-hold card — repeats at that card's interval until
pointer-up (`pointercancel`/`pointerleave` also stop it). Keyboard
activation (Enter/Space) still sends a single step per press. The timings
come from `GET /api/settings`, so the web remote's autorepeat always
matches the infrared remote's.

## Physical remote

If a key does not respond, open `http://<host>:8080/plugins/generic-input/`,
pick the device from the list ("Refresh" button if it was just plugged
in), click "Learn" on the action's row, press the key, then "Save". No
restart is needed: the table is re-read on every key press. To start from
a base, load the `mce` or `keyboard` preset.

## Config page

The former status page is now the **config page**, at
`http://<host>:8080/config` — `/status`, its historical URL, redirects
there, so existing bookmarks and links keep working. It lists the
plugins with their connection state and admin link, the audio output and
language pickers (below), the two settings cards described here, and the
recent error log.

A **sticky table of contents** sits alongside the cards (from the `lg`
breakpoint up): the entry for the section currently scrolled into view is
highlighted — an `IntersectionObserver` watches each section against a
band at the top of the viewport, and the first section still visible
there wins, so the highlight tracks what's actually being read rather
than whichever callback fired last. Clicking an entry scrolls smoothly to
its section.

### Audio output picker

Backed by `GET`/`PUT /api/audio-output`. The list comes straight from
`aplay -L`: each entry shows the device's **description** first, its
technical ALSA name in small print beneath — the `null` PCM (discards
audio) is filtered out, it has no place in an audio chain.

The first entry, **"System default"**, sends `device: null`: no device is
imposed on mpv (`audio-device=auto`), so the OS default applies. That is
the state of a fresh install — `audio_device` is `None` until a device is
explicitly picked — and it stays available afterwards to go back to
"whatever the system decides". A `PUT` with a named device persists it in
`state.json` and pushes it to mpv immediately; the currently selected
device is kept visible even if it disappears from `aplay -L` (a card
unplugged since), rather than leaving the picker blank.

### Startup card

Whether the device starts **on** (resumes the active source) or in
**standby** at launch — on by default. Persisted in `state.json`
(`settings.start_in_standby`) and read once at process start, so a device
configured for standby boot does not start playing again on its own after
a power cut or a reboot.

### Volume-hold card

The two timings that pace a **held** volume key, on the physical remote
and on the web remote's own buttons alike: the delay before the first
repeat, and the interval between the following ones. Backed by
`GET`/`PUT /api/settings`, with bounds enforced on write (a `PUT` outside
them answers `422`): initial delay **200-5000 ms**, interval **100-2000
ms**.

## Internationalization (i18n)

The interface is multilingual. The base language is **English**, embedded
in every binary; French (and other languages) are provided by **external
TOML packs**, decentralized per component:

    /etc/ritornello/locales/
      common/fr.toml   # shared vocabulary (play/pause/stop/error…)
      core/fr.toml     # core text + config page
      radio/fr.toml    # radio plugin + admin page
      cd/fr.toml       # cd plugin
      <third-party-plugin>/fr.toml

- Root configurable through `RITORNELLO_LOCALES` (default
  `/etc/ritornello/locales`).
- Language **picker** on the config page (`/config`): it lists `en` plus
  every `core/<lang>.toml` pack present, each language shown by its name
  in its own language ("Français", "English"). The change is applied live,
  pushed to the plugins, and persisted (`state.json`).
- **Adding a language**: copy the reference `en`, translate the values,
  drop it under `<root>/<component>/<lang>.toml`. A missing key or pack
  automatically falls back to English (per-key degradation, never an
  error). A pack that is present but unreadable (permissions, invalid
  TOML) is ignored **with a trace in the logs**.
- The initial French packs ship in `deploy/locales/` and are copied by
  `deploy/deploy.sh`.

## Theme

The interface offers a **light/dark** toggle and a picker opening a dialog
with the **42 themes** from [tweakcn](https://tweakcn.com) (Apache-2.0).
This is a **device** setting, like the language: it is persisted in
`state.json` (`theme` and `mode` fields) and therefore applies to every
browser visiting the interface. Default: `northern-lights`, light mode.

The fonts declared by the themes are loaded from a CDN — the interface's
only external resource. Offline, the display falls back to the system font
with no other consequence.

To regenerate the presets from upstream:
`cd web/kit && node scripts/fetch-presets.mjs`.
