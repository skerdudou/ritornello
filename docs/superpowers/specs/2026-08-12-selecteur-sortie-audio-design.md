# Audio output selector rework — design

Date: 2026-08-12. Status: approved (scope validated by the owner during the
2026-08-12 diagnostic session; design confirmed in the main session).

## Goal

Make the audio output selector on the config page honest and safe:

1. Show **human-readable descriptions** next to technical device names.
2. **Filter the `null` ALSA device** out of the list (it discards audio, and
   it was both first in the list and the SPA's preselection fallback — a
   distracted "Change" click muted the device).
3. Add a synthetic **"System default"** entry meaning "impose nothing on mpv,
   follow the OS default" — selected when no explicit choice is recorded,
   instead of preselecting an arbitrary first device.

## Background

- The list comes from `aplay -L` (`crates/ritornello-core/src/audio_output.rs`):
  each non-indented line is a selectable PCM name; the indented lines under it
  are its description, currently **discarded** by `parse_device_list`.
- `current: null` in `GET /api/audio-output` means "no explicit choice
  recorded" (`audio_device: None` in `state.json`): the core passes no
  `--audio-device` to mpv, which then follows the OS default. This is a
  legitimate state, not an error — but the UI previously hid it by
  preselecting the first listed device (usually `null`).
- mpv's `audio-device` property accepts `auto` (its native default) and is
  settable at runtime over IPC.

## API changes (`/api/audio-output`)

The SPA is the endpoint's only consumer and ships embedded in the core binary,
so the wire shape can change atomically.

**GET** returns device objects instead of bare strings:

```json
{
  "devices": [
    { "name": "sysdefault:CARD=Headphones", "description": "bcm2835 Headphones, bcm2835 Headphones — Default Audio Device" },
    { "name": "dmix:CARD=Headphones,DEV=0", "description": "bcm2835 Headphones, bcm2835 Headphones — Direct sample mixing device" }
  ],
  "current": null
}
```

- `description` = the device's indented `aplay -L` lines, trimmed and joined
  with `" — "`; empty string when there are none.
- The `null` ALSA device is filtered out by `parse_device_list` (by exact
  name match on `null`).
- `current` is unchanged: the persisted technical name, or JSON `null` when
  no explicit choice is recorded.

**PUT** accepts a nullable device:

- `{"device": "sysdefault:CARD=Headphones"}` — unchanged behavior (validated
  non-empty as today, 422 with `{"error": ...}` for empty/blank).
- `{"device": null}` — "follow the system default": the core sets
  `audio_device` to `None` (written as `"audio_device": null` in
  `state.json`, like the other optionals — no `skip_serializing_if`
  exception) and sends `audio-device=auto` to mpv, effective immediately.

Internally, the audio channel between the HTTP layer and the core loop
changes from `mpsc::Sender<String>` to `mpsc::Sender<Option<String>>`, and
`Core::set_audio_device` takes an `Option<String>` (`None` → `auto` to mpv,
`audio_device = None`, persist). At boot, `audio_device: None` keeps today's
behavior: nothing is sent to mpv, which defaults to `auto` — consistent.

## UI changes (audio card on the config page)

- A synthetic **"System default"** entry (i18n key `audio_default_device`,
  en: `"System default"`, fr: `"Par défaut (système)"`) sits first in the
  select, and is the selection when `current` is `null`. The entry is a
  view-level sentinel value — the literal string `"__system_default__"`,
  which cannot collide with an ALSA PCM name; it is never sent as-is —
  "Change" maps it to `{"device": null}`.
  The first-device preselection fallback disappears with its reason.
- Each real device renders **description first, technical name second**
  (muted, smaller), the same pattern as language names ("Français" shown,
  `fr` sent). A device without description renders its name alone.
- Robustness: when `current` names a device absent from the list (card
  unplugged), it is appended at the end (name only, empty description) so the
  current selection stays visible instead of an empty trigger.

## Testing

- Rust parse: descriptions kept and joined, `null` filtered, empty input,
  device without description.
- Rust HTTP: GET returns `{name, description}` pairs; PUT with `device: null`
  sends `None` on the channel and answers 204; PUT with `""` still 422 and
  changes nothing.
- Rust core: `set_audio_device(None)` sends `auto` to mpv and persists
  `audio_device: null`; `set_audio_device(Some(..))` unchanged.
- Vitest: default entry listed first and selected when `current: null`;
  choosing it PUTs `{"device": null}`; description/name rendering; absent
  current device appended; a real device change still PUTs its name.
- e2e: unchanged (the harness machine has no predictable ALSA devices).

## Out of scope

- Grouping or sorting devices, multi-output memory, any change to the
  `aplay -L` invocation itself.
- Renaming the endpoint or persisting anything new in `state.json`.
