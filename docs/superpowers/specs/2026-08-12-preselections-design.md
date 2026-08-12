# Real preset numbers and +10 access — design

Date: 2026-08-12. Status: approved (design and the "+10 / key 0 on the
physical remote" amendment validated by the owner in the main session; the
owner chose the shifted-window model for the web grid).

## Goal

1. The web preset grid shows **only the numbers that exist** for the active
   source: the radio's stations, the cd's tracks — instead of an
   unconditional 1–9.
2. Presets **beyond 9** become reachable, from the web (a `+10` button that
   shifts the grid window, auto-returning after a delay) and from the
   physical remote (new `+10` and `0` keys, with a cumulative tens offset
   held by the core and shown on the device display).
3. Rider (approved nice-to-have from the audio-selector review): on the
   config page, a failed `GET /api/audio-output` **disables the audio
   "Change" button** instead of silently offering to reset the device to
   "System default".

## Background

- `SourceMessage.preset: Option<u8>` already travels plugin → SDK →
  core → `PlayerState.preset` → SSE. It is the exact template for the new
  count field: proto struct, SDK builders (`SourceOutcome`, `Notification`),
  SDK client (`SourceUpdate`), core memory, `PlayerState`.
- Three hard 1–9 caps exist today and must be lifted:
  - radio station validation (`ValidationError::PresetOutOfRange`,
    `crates/ritornello-plugin-radio/src/config.rs`);
  - the cd plugin only declares `preset` for tracks 1–9
    (`issue()`, `crates/ritornello-plugin-cd/src/main.rs`);
  - generic-input binding validation rejects `Select` outside 1–9
    (`ValidationError::SelectOutOfRange`,
    `crates/ritornello-plugin-generic-input/src/bindings.rs`).
- `POST /api/command` builds an `InputMessage` and shares the **same
  channel** as physical input plugins; both land in `Core::handle_input`.
  The web and the remote are indistinguishable at the core.
- The core owns a single overlay slot (`overlay: Option<(View, Instant)>`,
  `OVERLAY = 2 s`) used by the volume/mute overlay and by transient source
  messages ("empty preset"). The tens offset reuses it.

## Source protocol: `preset_count`

`SourceMessage` gains a sibling of `preset`:

```rust
/// How many numbered presets the source currently offers: stations for the
/// radio, tracks for the cd. This is what lets the web UI show only the
/// numbers that exist instead of an unconditional 1-9 grid.
///
/// Absent = "this frame says nothing about the count, keep the previous
/// one". `Some(0)` is meaningful: "there is nothing to number" (cd without
/// a disc) — distinct from absent. The core forgets the remembered count on
/// source change and standby (the next source re-declares it on
/// activate/wake), but NOT on stop: a stopped radio still has its stations.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub preset_count: Option<u8>,
```

The field threads through the same four structs as `preset`:
`SourceMessage` (proto), `SourceOutcome` and `Notification` builders (SDK
server, new `.preset_count(n)` method), `SourceUpdate` (SDK client — the
reader's "is this frame interesting" gate adds `|| msg.preset_count.is_some()`).

**Radio** declares `preset_count` on every frame built by `play_preset`
(activate, select, next, prev) and on the empty-preset transient: the value
is the **highest preset number** in the station table
(`stations.iter().map(|s| s.preset).max().unwrap_or(0)`). Through the admin
UI presets are contiguous 1..N so max == len; a hand-edited sparse table
(presets 1, 5, 9) yields max = 9 and the grid shows some numbers that answer
with the existing "empty preset" transient — graceful, no new machinery.
Station validation bound is raised from 1..=9 to **1..=99** (two decades of
+10 presses; the validation message texts follow, en + fr).

**Cd** declares `preset_count` on every frame built by `issue()`:
`total_tracks` when a TOC is known, `0` otherwise (no disc, or TOC still
being read — a brief empty grid while the TOC loads is honest). The `(1..=9)`
guard on declaring `preset` is removed: any track number is declared
(`u8::try_from`, tracks beyond 255 declare nothing — irrelevant for audio
CDs, which cap at 99).

A count edited behind the source's back (radio admin adding a station) is
reflected on the next interaction with the source, not pushed live —
accepted.

## Core: count memory and `PlayerState`

- New `Core` field `preset_count: Option<u8>`, updated from
  `SourceUpdate.preset_count` in `handle_source_update` (same shape as
  `preset`).
- Forgotten on `Command::SourceCycle` and on `Command::Power` entering
  standby — explicitly, in those match arms. **Not** wired into
  `set_identity(None)`: stop and `plays_nothing` clear `preset`, never the
  count.
- `PlayerState` gains `pub preset_count: Option<u8>` (serialized always,
  `null` when unknown, like `preset`), copied in `etat_lecteur()`. The SPA's
  `PlayerPayload` gains `preset_count: number | null`.

## Command protocol: remote `+10` and key `0`

- `Command` gains a **`Plus10`** variant (no argument, like `Mute`). It
  serializes as `{"cmd":"Plus10"}` and reads `cmd = "Plus10"` in binding
  TOML — no plumbing change anywhere (bindings flatten `Command`, the HTTP
  endpoint and input channel carry any `Command`).
- `Command::Select(0)` becomes legal input: it is the remote's `0` key.
  generic-input validation widens to `0..=9` (`select_out_of_range` message
  texts updated to "0-9", en + fr).
- generic-input admin `ACTIONS` gains two assignable actions:
  `act_select_0` → `Select(0)` and `act_plus10` → `Plus10`, with i18n keys
  in the plugin's `en.toml` and `deploy/locales/generic-input/fr.toml`
  (en: "Key 0" / "+10"; fr: "Touche 0" / "+10"). All four action-count
  guards move from 19 to 21 (preset-toml test, InputAdmin test, i18n guard
  via `ACTIONS`, e2e `data-action-row` count).

### Pending tens offset (core)

New `Core` field `pending_tens: u8` (0 = none), handled in
`appliquer_commande`:

- **`Plus10`**: `pending_tens += 10`, wrapped by the known count: the
  highest useful offset is `(count / 10) * 10` — the largest multiple of 10
  that is still a reachable number (with 20 stations, station 20 is
  `+10 +10` then `0`, so offset 20 must be allowed); going past it wraps to
  0 (mirrors the web window cycle). With no known
  count, the offset saturates at 240. While the offset is non-zero, the
  overlay shows it — line1 = catalog key `preset_label` (en `"PRESET"`,
  fr `"PRESELECTION"`), line2 = `"+10"` / `"+20"`… — in the **same slot and
  with the same `OVERLAY` deadline** as the volume overlay; each press
  re-arms the deadline. A wrap to 0 clears the overlay (permanent view
  reappears).
- **`Select(d)`**: the effective number is `pending_tens + d`, and the
  offset is consumed (reset to 0). An effective 0 (key `0` with no pending
  offset) is silently ignored. Numbers beyond the count follow the existing
  path: the source answers with its "empty preset" transient.
- **Any other command** resets `pending_tens` to 0 (a volume press
  mid-sequence abandons the sequence). Overlay expiry
  (`expire_overlay`) also resets it: the offset's lifetime is the
  overlay's lifetime — one deadline, not two timers.
- Standby and source change reset it (covered by "any other command" for
  `Power`/`SourceCycle`).
- `held` on `Plus10` is ignored like every held non-volume command
  (no autorepeat on +10).

Known race, accepted: the web sends absolute `Select(n)` on the same
channel, so a web click while a remote offset is pending combines with it
(`Select(3)` + pending 10 → 13). Two users acting within the same 2 s
window on two devices — not worth machinery to distinguish the origins.

## Web UI: shifted-window preset grid (HomeView)

Driven by `etat.preset_count`; local state: `fenetre` (0-based window
index) and a reset timer.

- **Numbers shown** (only those ≤ count): window 0 → `1..9`; window w ≥ 1 →
  `10w..10w+9` (ten numbers). This matches remote reachability exactly:
  *k* presses of `+10` then digit `d` reach `10k + d`, and `10k + 0` covers
  10, 20… So 23 stations: `1–9`, `10–19`, `20–23`.
- **`+10` button** (last grid cell, `data-preset-plus10`, label `+10`):
  shown only when `count > 9`. Each press advances `fenetre`, wrapping to 0
  past the last window (same rule as the core offset), and re-arms the
  reset timer.
- **Auto-return**: the timer resets `fenetre` to 0 after **2000 ms**
  without a press — the same value as the core's `OVERLAY` constant,
  duplicated in the SPA with a comment naming the constant. Selecting a
  number sends `Select(n)` (absolute, never `Plus10`), resets `fenetre` to
  0 immediately and clears the timer.
- **Fallbacks**: `preset_count` null/undefined (source predating the field,
  or nothing declared) → today's static 1–9 grid, no `+10`, no timer — the
  remote never goes mute. `preset_count` 0 → no digit buttons at all.
- **Active highlight**: unchanged predicate (`etat.preset === n`), now
  meaningful beyond 9; the active button is highlighted when its window is
  visible.
- Existing attributes preserved: `data-preset-button="n"`,
  `data-preset-active`, `aria-current`, variant default/secondary. The
  virtual remote rows (`REMOTE_ROWS`/`REMOTE_COMMANDS`) are untouched — the
  grid is the digit interface.

## Rider: audio "Change" disabled on failed GET

`ConfigView` tracks the audio load failure (a ref set when
`GET /api/audio-output` rejects); the `[data-audio-change]` button gets
`:disabled` from it. Without this, a failed GET leaves the select on
"System default" as if it were the real state, and "Change" would silently
send `{"device": null}` — a reset the user never asked for.

## Testing

- **proto**: `Plus10` roundtrip (`{"cmd":"Plus10"}`), `Select(0)` roundtrip,
  `preset_count` roundtrip + absent-by-default backward compatibility
  (mirror of the `preset` test).
- **SDK**: builder `.preset_count(n)` lands in the emitted `SourceMessage`;
  client maps it into `SourceUpdate` and treats a count-only frame as
  interesting.
- **core**: count remembered from updates, forgotten on SourceCycle and
  standby, kept on stop; published in `PlayerState` (SSE dedup untouched).
  Offset: accumulation, wrap with known count, saturation without count,
  consumption by `Select`, key 0 alone ignored, other commands reset it,
  overlay expiry resets it, overlay text `+10`/`+20` with `preset_label`,
  re-armed deadline.
- **radio**: frames declare the max-preset count; validation accepts
  preset 42, rejects 100; empty-preset transient still declares the count.
- **cd**: frames declare `total_tracks` with a TOC, 0 without; track 12 now
  declares `preset: 12`.
- **generic-input**: validation accepts `Select(0)`, still rejects
  `Select(10)`; `Plus10` binding roundtrips in TOML; ui ACTIONS = 21 and
  i18n keys present.
- **Vitest HomeView** (fake timers): only existing numbers per window; `+10`
  hidden at count ≤ 9; window shift, wrap, 2 s auto-return, selection
  resets the window; fallback null → 1–9; count 0 → nothing; highlight
  beyond 9 (`preset: 14` in window 1).
- **Vitest ConfigView**: `monter({ '/api/audio-output': undefined })` →
  `[data-audio-change]` disabled.
- **e2e**: the harness radio declares one station → the home grid shows
  exactly one digit button (`[data-preset-button="1"]`), no `+10`; the
  generic-input admin `data-action-row` count moves to 21.

## Out of scope

- Live push of a count edited via the radio admin (reflected on next
  interaction).
- Any change to the device display beyond the `+NN` overlay (no windowed
  grid on the hardware display).
- New buttons on the web virtual remote rows; IR learning page changes
  beyond the two new assignable actions.
- Persisting the pending offset or the web window anywhere.
