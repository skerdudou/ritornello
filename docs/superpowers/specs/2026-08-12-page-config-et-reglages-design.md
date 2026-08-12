# Config page and behavior settings — design

Date: 2026-08-12. Status: approved.

## Goal

Four small changes, all centered on the status page:

1. Rename the **status** page to **config** (with proper i18n labels).
2. Add a **sticky, clickable table of contents** (scrollspy) on that page.
3. **Volume hold-to-repeat**: holding Volume +/- (web buttons *and* physical
   remote) starts stepping the volume after an initial delay of 1000 ms, then
   every 500 ms until release. Both timings configurable on the config page.
4. A **startup power state** setting: on or standby when the program launches
   (on by default), configurable on the config page.

## Approach for volume repeat (chosen: A)

The core owns the timings; each consumer stays simple.

- Timings live in `state.json` and are exposed by a new `GET/PUT /api/settings`
  endpoint, edited on the config page.
- **Web buttons**: press-and-hold (pointerdown/pointerup) with browser-side
  timers, timings fetched from `/api/settings`. The page sends ordinary
  `VolumeUp`/`VolumeDown` commands at the right cadence — the core applies each
  one as today.
- **Physical remote**: `generic-input` currently keeps only key-down events
  (`value == 1`, `devices.rs`). It will also forward kernel autorepeat events
  (`value == 2`) for keys bound to `VolumeUp`/`VolumeDown`, marking the line
  `"held": true`. The **core** paces them: held commands are ignored until the
  initial delay has elapsed since the initial (non-held) step, then one is let
  through per interval. No timer in the plugin, so **volume cannot run away if
  a key-up is lost** — no kernel repeats means nothing moves.

Rejected: a bidirectional input protocol pushing timings to the plugin. More
invasive (SDK + protocol + resync on change), and the web would still need its
own timers.

### Wire format

The input protocol line grows an optional field, backward compatible
(absent = `false`):

```json
{"cmd": "VolumeUp", "held": true}
```

In `ritornello-proto`, a small envelope wraps the existing `Command`
(`#[serde(flatten)]` + `#[serde(default)] held: bool`), used by the input
protocol. Existing plugins that write plain `Command` lines keep working.
`held` on any command other than `VolumeUp`/`VolumeDown` is ignored by the
core. The SDK's `InputPlugin::next_command` returns the envelope.

### Core pacing

One deadline field: on a non-held volume step, `deadline = now + delay`; on a
held volume command, step only if `now >= deadline`, then
`deadline = now + interval`. Held commands never wake the device from standby
(same guard as today: everything except `Power` is blocked in standby).

## Settings storage and API

`PersistedState` gains a `#[serde(default)]` settings struct (older
`state.json` files load unchanged):

```json
{
  "volume_repeat_initial_ms": 1000,
  "volume_repeat_interval_ms": 500,
  "start_in_standby": false
}
```

- `GET /api/settings` returns the struct; `PUT /api/settings` validates bounds
  and answers 422 outside them (same pattern as `validate_audio_device`):
  initial delay 200–5000 ms, interval 100–2000 ms.
- Startup: when `start_in_standby` is true, the core starts with
  `standby = true` — volume and audio device are still applied to mpv, but no
  `Wake` is sent to the active source, and the display shows the standby view.

## Config page (rename + TOC + new cards)

- Route `/config`, with a redirect `/status` → `/config` (consistent with the
  existing historical-URL policy). `StatusView.vue` → `ConfigView.vue` (and its
  test). The `/api/status` endpoint keeps its name — it reports plugin status,
  which is still what it does.
- i18n: new key `config_title` ("configuration" / « configuration ») for the
  nav tab and page. The plugins table card gets its own title key
  (`plugins_title`, "Plugins") since `status_title` disappears. Keys added to
  the embedded `en.toml` and `deploy/locales/core/fr.toml`; the
  `i18nKeysUsed` guard keeps both catalogs honest.
- Two new cards: **Startup** (on / standby select, on by default) and
  **Volume hold** (two numeric inputs, ms), each with a save button posting to
  `PUT /api/settings`.
- **Table of contents**: one entry per card, in a sticky `<aside>` shown on
  large screens only (the page column is `max-w-3xl`; no room on mobile).
  Active section tracked with an IntersectionObserver; click scrolls smoothly
  to the card. Labels reuse the cards' title keys.

## Web remote hold

On the home page's Volume +/- buttons: pointerdown sends the first command and
arms the timers (initial delay, then interval); pointerup / pointercancel /
pointerleave stop them. Timings fetched from `/api/settings` when the page
mounts. Other remote buttons keep plain clicks.

## Testing

- Rust: pacing of held commands (before delay / after delay / interval),
  settings bounds (422), `start_in_standby` startup path (no `Wake`, standby
  view), state round-trip with and without the settings block.
- Vitest: ConfigView rename, new cards save flow, TOC rendering and click,
  HomeView hold-to-repeat with fake timers, envelope parsing.
- e2e: update `/status` → `/config` in `parcours.spec.ts`; a hold gesture is
  not added to e2e (timer-dependent, covered by unit tests).

## Out of scope

- Hold-to-repeat on other commands (Next/Prev, mute).
- Pushing settings to plugins over the socket.
- Any change to the learn UI or bindings file format.
