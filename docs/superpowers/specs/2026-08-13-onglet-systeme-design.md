# System tab: OS metrics and power actions — design

Date: 2026-08-13. Status: approved (design validated by the owner in the
main session, together with two additions they requested: restarting the
Ritornello service, and an in-page CPU/RAM history graph that starts empty
on every arrival).

## Goal

A new **System** tab in the web UI, carrying three things:

1. **OS metrics**: SoC temperature, CPU frequency, load average, memory,
   root-filesystem usage, Raspberry Pi undervoltage, OS and service
   uptimes, and the device's identity (hostname, IP, OS, kernel, Ritornello
   version).
2. A **small CPU/RAM history graph**, local to the page: it fills as the
   page polls and is deliberately **empty on every arrival** — no
   server-side history, nothing persisted.
3. **Power actions**: power off the OS, reboot the OS, and restart the
   Ritornello service — each behind a confirmation dialog.

## Background: what the repository already establishes

- **No system metrics exist today.** This is greenfield; nothing to
  refactor.
- The service runs as the unprivileged `ritornello` user with
  `NoNewPrivileges=true` (`deploy/ritornello.service`). `sudo` and any
  setuid path are therefore **structurally unavailable** — not a policy
  choice to revisit but a property of the unit.
- `ProtectSystem=strict` mounts the hierarchy read-only. It does **not**
  prevent connecting to `/run/dbus/system_bus_socket`: the kernel's
  read-only check (`sb_permission`) rejects writes to regular files,
  directories and symlinks — sockets are exempt. `systemctl` and `busctl`
  therefore work from inside the unit.
- The unit already carries `Restart=always` and `RestartSec=2`.
- `Core::persist()` is called on **every** change
  (`crates/ritornello-core/src/core.rs:786`, six call sites) — nothing is
  buffered for shutdown, so an abrupt process exit loses no state. This is
  what makes the service-restart mechanism below safe.
- `crates/ritornello-core/src/audio_output.rs` is the template for this
  kind of module: a **pure parse function** with unit tests, a thin I/O
  wrapper that spawns a command and is *not* unit-tested, and handlers in
  `status.rs`.
- HTTP validation pattern: validate first, return
  `422` + `{"error": "<message>"}` on refusal, change nothing. The SPA's
  `api` helper turns that `error` field into a toast.
- `libc` is already in `Cargo.lock` (tokio depends on it), so using it in
  the core adds no compilation.
- Language policy: docs and new Rust comments in English, Rust test names
  and SPA comments in French, user-facing strings via i18n with every key
  present in **both** `crates/ritornello-core/src/locales/en.toml` and
  `deploy/locales/core/fr.toml` (the SPA's `i18nKeysUsed` guard reads the
  former).

## Where the code lives

In the **core**, not in a plugin: `crates/ritornello-core/src/system.rs`.

A `system` admin plugin was considered and rejected: it would have to be
added by hand to the `plugins.toml` of every existing installation —
`deploy.sh` never overwrites that file, so the feature would stay invisible
after an update — and OS metrics are neither a source nor an input, so they
have no business behind the plugin protocol.

New surface:

| Piece | Path |
|---|---|
| Metrics, capability probe, power actions, handlers | `crates/ritornello-core/src/system.rs` (new) |
| Route registration | `crates/ritornello-core/src/status.rs` |
| Startup probe, `SystemInfo` construction | `crates/ritornello-core/src/main.rs` |
| polkit rule | `deploy/50-ritornello-power.rules` (new) |
| Rule installation | `deploy/deploy.sh` |
| View | `web/app/src/views/SystemView.vue` (new) |
| Sparkline path builder | `web/app/src/views/sparkline.ts` (new) |
| Route + nav link | `web/app/src/router.ts`, `web/app/src/App.vue` |
| Payload type | `web/app/src/types.ts` |

## `GET /api/system`

Every metric is optional and **`null` when unreadable** — a request never
fails because a file is absent. An x86 machine has no `rpi_volt` sensor,
WSL has no thermal zone, a VM often has no `cpufreq`; all three must render
a page, not an error.

```json
{
  "temperature_c": 47.8,
  "cpu_mhz": 900,
  "load": [0.12, 0.15, 0.09],
  "cpus": 4,
  "memory": { "total_kb": 948000, "available_kb": 512000 },
  "disk": { "total_kb": 30000000, "available_kb": 24000000 },
  "under_voltage": false,
  "uptime_s": 84213,
  "service_uptime_s": 3600,
  "hostname": "ritornello",
  "ip": "192.168.1.20",
  "os": "Debian GNU/Linux 12 (bookworm)",
  "kernel": "6.6.51+rpt-rpi-v7",
  "version": "0.1.0",
  "can_power_off": true,
  "can_reboot": true
}
```

Types: `temperature_c: Option<f32>` · `cpu_mhz: Option<u32>` ·
`load: Option<[f32; 3]>` · `cpus: Option<usize>` ·
`memory`/`disk`: `Option<Usage { total_kb: u64, available_kb: u64 }>` ·
`under_voltage: Option<bool>` · `uptime_s: Option<u64>` ·
`service_uptime_s: u64` · `hostname`/`ip`/`os`/`kernel`: `Option<String>` ·
`version: String` · `can_power_off`/`can_reboot`: `bool`.

`Option` fields serialize as `null` (no `skip_serializing_if`): the SPA
distinguishes "unreadable" from "absent from an older core" nowhere, and a
stable key set keeps the view simple.

**There is deliberately no `can_restart_service` field.** Restarting the
service needs no privilege (see below), so it is always available; a
capability flag would invite the reader to think otherwise.

### Sources and pure parsers

| Field | Source | Pure function |
|---|---|---|
| `temperature_c` | `/sys/class/thermal/thermal_zone0/temp` (millidegrees) | `parse_temperature(&str) -> Option<f32>`, one decimal |
| `cpu_mhz` | `/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq` (kHz) | `parse_khz_to_mhz(&str) -> Option<u32>` |
| `load` | `/proc/loadavg` | `parse_loadavg(&str) -> Option<[f32; 3]>` |
| `cpus` | `std::thread::available_parallelism()` | — |
| `memory` | `/proc/meminfo` (`MemTotal`, `MemAvailable`) | `parse_meminfo(&str) -> Option<Usage>` |
| `disk` | `libc::statvfs("/")` | — (I/O; see below) |
| `under_voltage` | `in0_lcrit_alarm` of the `/sys/class/hwmon/hwmon*` whose `name` is `rpi_volt` | `parse_alarm(&str) -> Option<bool>` |
| `uptime_s` | `/proc/uptime` (first field) | `parse_uptime(&str) -> Option<u64>` |
| `service_uptime_s` | `SystemInfo.started.elapsed()` | — |
| `hostname` | `/proc/sys/kernel/hostname` | trim |
| `ip` | local address of a UDP socket "connected" to `8.8.8.8:53` | — |
| `os` | `/etc/os-release` `PRETTY_NAME` | `parse_os_release(&str) -> Option<String>`, quotes stripped |
| `kernel` | `/proc/sys/kernel/osrelease` | trim |
| `version` | `env!("CARGO_PKG_VERSION")` | — |

Three points a reviewer will (rightly) question, settled here:

- **The reads are synchronous `std::fs` calls inside an async handler, and
  that is correct.** These are procfs/sysfs pseudo-files generated by the
  kernel on read: they do not wait on a device the way a disk read does.
  `statvfs` on the root filesystem reads a cached superblock. Neither
  warrants `spawn_blocking`.
- **The UDP socket sends nothing and needs no internet.** `bind` +
  `connect` on a datagram socket only asks the kernel which local address
  the route to that destination would use; no packet leaves. Failure (no
  route at all) yields `None`. The address shown is the one facing the
  default route — a multi-homed device shows one address, which is the
  useful one.
- **`under_voltage` is unverified on the owner's hardware.** The `rpi_volt`
  hwmon driver exposes `in0_lcrit_alarm` on recent kernels; `vcgencmd` was
  rejected because it needs the `video` group and a spawned process per
  poll. If the sensor is absent the field is `null` and the UI shows
  nothing at all — no error, no empty warning box. Verifying it on the
  device is listed under Hardware checks.

## `POST /api/system/power`

Body: `{"action": "poweroff" | "reboot" | "restart-service"}`.

The action is read as a `String` and validated by a pure
`parse_action(&str) -> Result<PowerAction, String>`, not by serde's enum
deserialization: an unknown value must produce the project's
`422` + `{"error": …}` shape, whereas a serde rejection would produce
axum's plain-text 422. Same reasoning as `validate_audio_device`.

Our own messages are French (like `validate_audio_device`'s). Text relayed
from `logind` stays verbatim — its precision is the whole point.

### `poweroff` / `reboot`

Spawn `systemctl poweroff` / `systemctl reboot` (via `SystemInfo.systemctl`,
see Injection points) and **await the child up to 5 s**:

| Outcome | Response |
|---|---|
| exit 0, or still running after 5 s | `202` + `{}` — the machine is going away |
| non-zero exit | `502` + `{"error": <trimmed stderr, else "systemctl a échoué (code N)">}` |
| spawn failed | `500` + `{"error": …}` |

Surfacing the stderr is not decoration: without polkit configured, logind
answers `Interactive authentication required`, which is exactly the
sentence that tells the owner what to fix. A fire-and-forget 202 would
show a page that looks fine while nothing happens.

No client D-Bus library: that would be `zbus`, a large dependency for two
calls, in a project that already spawns `mpv`, `aplay` and `eject`.

### `restart-service`

Respond `202` immediately, then, from a detached task, wait 300 ms and
**exit the process** — `systemd` restarts it 2 s later because our own unit
says `Restart=always` / `RestartSec=2`.

Chosen over `systemctl restart ritornello.service` because it needs **no
privilege at all**: the button works on an installation where polkit is
missing, it adds no `org.freedesktop.systemd1.manage-units` rule, and it
avoids the self-restart race where `systemctl` is killed by the very unit
stop it requested before it can report anything. `persist()` runs on every
change, so exiting without unwinding loses nothing.

The 300 ms delay exists so the `202` reaches the browser before the socket
dies. Two consequences to document, not to fix:

- run **outside** systemd (development), the action simply stops the
  process — restarting is the supervisor's job, and there is none;
- systemd's start rate limit (`DefaultStartLimitBurst=5` per 10 s) applies:
  hammering the button five times in ten seconds leaves the unit failed and
  *not* restarted. The confirmation dialog plus the in-progress lockout
  make that a deliberate act rather than an accident.

## Power capability probe

At startup, `main.rs` asks logind whether it is allowed, once:

    busctl --system call org.freedesktop.login1 /org/freedesktop/login1 \
      org.freedesktop.login1.Manager CanPowerOff

Output `s "yes"` → `true`; `"no"`, `"challenge"`, anything else, a non-zero
exit, a missing `busctl` or a 3 s timeout → `false`, logged at INFO with
the reason (INFO, not WARN: a development machine without logind is normal
and must not fill the page's log buffer). Same for `CanReboot`. Pure
`parse_can(&str) -> bool` covers the parsing.

The result is **cached for the process lifetime, on purpose**: two spawned
processes per 5-second poll would be absurd. Installing the polkit rule
therefore takes effect on the next service start — and `deploy.sh` restarts
the service, which is how the rule gets installed in the first place.

The cache does not replace the stderr surfacing above. logind can still
deny at call time (a second session, an inhibitor), which is also why the
rule below grants all six actions.

## polkit rule

`deploy/50-ritornello-power.rules`, installed by `deploy.sh` into
`/etc/polkit-1/rules.d/` (root-owned, `0644`, always overwritten — it is
our file, like the systemd unit; `mkdir -p` first, so a machine where
polkit is not yet installed still receives it).

The rule grants the `ritornello` user **three actions per verb**:

    org.freedesktop.login1.power-off
    org.freedesktop.login1.power-off-multiple-sessions
    org.freedesktop.login1.power-off-ignore-inhibit
    org.freedesktop.login1.reboot
    org.freedesktop.login1.reboot-multiple-sessions
    org.freedesktop.login1.reboot-ignore-inhibit

logind checks the base action only when nothing else is going on: it
switches to `*-multiple-sessions` as soon as another session exists — an
open SSH connection is enough, which is precisely the situation the owner
will be in while testing — and to `*-ignore-inhibit` when an inhibitor is
held. Granting only the base action produces "it works, except when I am
logged in over SSH", a symptom that costs an hour to attribute. The rule's
comment says so, in the file.

`deploy.sh` does not install the polkit **package** and does not probe for
it: it installs no package today (the docs list `apt install mpv cd-discid
eject`), and the diagnosis is already carried by the disabled buttons plus
the documentation section.

## The tab

Route `/system`, nav link `t('system_title')` in `App.vue` after
`config`, view `SystemView.vue`.

Cards, in order (reordered after the owner tried the original layout — see
Amendments):

1. **CPU** — temperature, CPU frequency, core count, and a real CPU usage
   percentage (see History below for how it's computed — it is *not* the
   load average, which moved to the History card).
2. **Memory** — used / total, with a bar.
3. **History** — the CPU/RAM sparkline, the load average (moved here from
   the original CPU/Load card: it complements the CPU series it sits next
   to, not the instantaneous reading in the CPU card), and the window-label
   / refresh-period controls (below).
4. **Storage** — used / total of `/`, with a bar.
5. **Device** — hostname, IP, OS, kernel, Ritornello version, OS uptime,
   service uptime, and a **permanent three-state supply-voltage row** (no
   sensor / nominal / undervoltage — see Amendments; it used to be a
   Power-card warning shown only when `under_voltage` was `true`).
6. **Power** — the three buttons.

Unreadable values render as `—`. A failed `GET /api/system` shows a
diagnostic line and logs to the console — **no toast**: a poll repeating
every 5 s would turn one unreachable core into a stream of toasts. The
config page's `audioIndisponible` flag is the model. The page is never left
blank.

**Wording is load-bearing:** the buttons read « Éteindre le système » and
« Redémarrer le système », never « veille ». The home page already offers
standby; confusing the two would be the worst possible misreading of this
tab. The service button reads « Redémarrer Ritornello » and its dialog says
the device stays on.

### Polling

`GET /api/system` on a **user-selectable period** — 1, 2, 5, 10, or 30 s,
defaulting to 5 s (see Amendments; the original design fixed it at 5 s) —
only while the view is mounted **and** the document is visible
(`visibilitychange`); stopped on unmount. The period is a page-local `ref`,
not a device setting: it lives in neither `localStorage` nor
`/api/settings`, and resets to 5 s on every arrival, like the history
itself.

A poll cannot overlap another (an in-flight guard makes a second `sonder()`
call a no-op), and a period change aborts whatever poll is still in flight
via `AbortController` before starting a fresh one at the new rhythm. Both
guards exist because the CPU percentage below is **stateful**: before it
existed, a late response only meant a stale display; now, a response that
lands out of order would overwrite the jiffy reference the next delta is
computed against, corrupting it rather than merely delaying it.

This diverges from the project's documented "volatile state goes through
SSE" doctrine, and the divergence is deliberate — the view carries a
comment saying why, where a reader will ask: the player's SSE stream
publishes state the core produces anyway, whereas these metrics exist only
because someone asked for them. Pushing them would make an idle appliance
work permanently for nobody.

### History graph

- `historique = ref<{ cpu: number; ram: number; t: number }[]>([])`, capped
  at **60 samples**, one push per successful poll. `t = Date.now()` was
  added after the original design (see Amendments) for the hover popin and
  the window label below.
- **`cpu` is a real CPU utilisation percentage, computed in the page, not in
  the core** — the original design's `min(100, load[0] / cpus * 100)` was
  replaced (see Amendments) because the load average is a minutes-long
  smoothed figure that barely moves between two polls a few seconds apart
  and reads as flat, especially at the faster end of the new refresh-period
  range. The core instead exposes two cumulative jiffy counters from
  `/proc/stat` (`cpu_total_jiffies`, `cpu_idle_jiffies` — see `Metrics`
  above), and the page differences two successive polls itself:
  `100 × (1 − Δidle / Δtotal)`, clamped to 0–100, `null` when there is no
  previous poll yet or when `Δtotal <= 0`. **The core does not compute the
  percentage itself, deliberately**: that would need it to remember the
  previous reading as shared state, and two browser tabs polling out of
  phase would corrupt each other's delta — letting the page difference its
  own successive polls avoids that altogether. `ram` is unchanged:
  `(total_kb - available_kb) / total_kb * 100`. A sample is pushed only
  when **both** are computable; a machine missing readable jiffies or
  `meminfo` keeps an empty graph rather than a half-drawn one — and, since
  the CPU delta itself needs a previous reading, the very first poll after
  every arrival (and after every period change) never pushes a sample.
- Rendered as an inline `<svg viewBox="0 0 100 30" preserveAspectRatio="none">`
  with two `<path fill="none" stroke="currentColor">`, coloured by Tailwind
  classes so the themes apply (`text-primary` for CPU,
  `text-muted-foreground` for RAM). Both paths carry
  `vector-effect="non-scaling-stroke"`: without it,
  `preserveAspectRatio="none"` stretches the stroke width along with the
  geometry.
- Path geometry comes from a pure, unit-tested
  `cheminSparkline(valeurs: number[], largeur: number, hauteur: number): string`
  in `web/app/src/views/sparkline.ts`: values clamped to 0–100, y inverted
  (0 % at the bottom), `''` for fewer than two points.
- Below the graph, a legend with the two current percentages, and the load
  average (moved here — see the card order above).
- **Empty on every arrival, by design** (the owner asked for exactly this):
  the array is a local `ref`, never persisted, never server-side. With
  fewer than two samples the card shows
  `t('system_history_empty')` — "fills as the page polls" — instead of an
  empty frame. A metrics history is not state worth keeping on an appliance
  that is idle most of the time.
- **Refresh-period selector** (added after the original design — see
  Amendments): 1/2/5/10/30 s, defaulting to 5 s (see Polling above).
  Choosing the period already in effect is a no-op — it does not restart
  polling or reset the delta window.
- **Window label**: `t('system_history_span', { minutes })` states the span
  actually covered by `historique`, derived from the timestamp of its first
  and last sample — **not** `CAPACITE × période`, which only matches
  reality once the buffer has filled *at the current period*; switching
  period with a full buffer, or simply arriving on the page, made that
  figure wrong for up to 60 polls. The capacity-based figure is now only a
  fallback, used while fewer than two samples exist to measure a real span
  from.
- **Hover / touch readout** (added after the original design — see
  Amendments): pointer events over the graph draw a vertical line
  (`data-system-history-line`) and a small popin naming the pointed
  sample's timestamp and both percentages, positioned by nearest column
  (`Math.round`) and horizontally clamped in pixels so it cannot spill past
  either edge of the card. `pointermove` covers both mouse hover and touch
  drag; `pointerdown` covers a still tap (which would otherwise never fire
  `pointermove`); `pointerleave` and `pointercancel` clear it — a
  `pointerup` handler was tried and removed, since a direct-manipulation
  pointer already fires `pointerleave` right after its `pointerup`.

### Power card behaviour

State: `enCours: null | 'poweroff' | 'reboot' | 'restart-service'`.

- Each button opens a `Dialog` (already exported by `@ritornello/ui`) with
  its own wording and Cancel / Confirm. Nothing is sent before Confirm.
- « Éteindre » / « Redémarrer » are **disabled when `can_power_off` /
  `can_reboot` is false**, with a line explaining that polkit is not
  configured and pointing at the documentation — the pattern of the audio
  « Changer » button.
- After a confirmed `poweroff`/`reboot`: polling stops, the card shows
  « l'appareil s'éteint » / « redémarre », all three buttons stay disabled.
  This state is terminal until the user reloads — the server is gone and
  cannot say anything more. Without it, the next poll would fail and show
  an alarming network error while everything is going exactly as asked.
- After a confirmed `restart-service`: remember the current
  `service_uptime_s`, then poll every **2 s ignoring errors** (the service
  is down; that is expected) until a response comes back with a *smaller*
  `service_uptime_s` — comparing uptimes, not merely "a response arrived",
  because the first poll may still reach the old process. Then toast
  « Ritornello a redémarré » and resume normal polling. After **30 s**
  without that, toast « le service ne répond pas » (pointing at
  `journalctl`) and resume normal polling anyway.

## Injection points (and why they exist)

`AppState` gains **one** field, `system: Arc<crate::system::SystemInfo>`,
so the five test constructors in `status.rs::tests_support` grow by one line
each rather than several:

```rust
pub struct SystemInfo {
    pub started: std::time::Instant,
    pub can_power_off: bool,
    pub can_reboot: bool,
    /// Command used for the OS power actions. A field, not a constant,
    /// **so the destructive routes can be tested at all**: a test that
    /// really ran `systemctl poweroff` would shut down the machine running
    /// the test suite. Tests point it at `/bin/true` and `/bin/false` and
    /// exercise the real spawn/await/exit-code path.
    pub systemctl: String,
    pub restart_delay: std::time::Duration,
    /// Called to restart the service. Default: `std::process::exit(0)`,
    /// which a test cannot call — it would kill the test binary.
    pub restart: Arc<dyn Fn() + Send + Sync>,
}
```

`Default`: `started` = now, capabilities `false`, `systemctl` =
`"systemctl"`, `restart_delay` = 300 ms, `restart` = `|| exit(0)`.

Both injection points exist for one reason — making routes that destroy
their environment testable — and the code says so. Nothing else in the
module is abstracted.

## i18n

Every new key goes into **both** `crates/ritornello-core/src/locales/en.toml`
(English, checked by the SPA's `i18nKeysUsed` guard) and
`deploy/locales/core/fr.toml` (French). Keys, all prefixed `system_`:

`system_title`, `system_cpu`, `system_cpu_usage`, `system_temperature`,
`system_frequency`, `system_loadavg`, `system_cores`, `system_period`,
`system_unit_second`, `system_history`, `system_history_empty`,
`system_history_span`, `system_memory`, `system_storage`,
`system_device`, `system_hostname`, `system_ip`, `system_os`,
`system_kernel`, `system_version`, `system_uptime`,
`system_service_uptime`, `system_voltage`, `system_voltage_ok`,
`system_voltage_low`, `system_power`, `system_poweroff`,
`system_reboot`, `system_restart_service`, `system_confirm_poweroff`,
`system_confirm_reboot`, `system_confirm_restart_service`,
`system_confirm`, `system_cancel`, `system_power_unavailable`,
`system_under_voltage`, `system_powering_off`, `system_rebooting`,
`system_restarting`, `system_restarted`, `system_restart_timeout`,
`system_unavailable`.

(This list was amended 2026-08-14 to match what actually shipped: the
original `system_load` never existed under that name — the load-average key
is `system_loadavg` — and it omitted every key added since, listed above:
`system_cpu`, `system_cpu_usage`, `system_period`, `system_unit_second`,
`system_history_span`, `system_voltage`, `system_voltage_ok`,
`system_voltage_low`. `system_under_voltage` is no longer the Device grid's
row value — it is now a full sentence shown as a separate advice line,
`role="status"`, directly under the grid and only while the alert is
active; the row itself shows the short `system_voltage_low` /
`system_voltage_ok`, see Amendments.)

Units are translated too, rather than hardcoded in the view — the SPA is
bilingual and "MB"/"Mo" and "d"/"j" differ: `system_unit_mb`,
`system_unit_gb`, `system_unit_day`, `system_unit_hour`,
`system_unit_minute`.

## Documentation

- **`docs/installation.md`** — one new section, "Shutdown and reboot from
  the web UI": what `deploy.sh` installs, the polkit prerequisite with
  **one line per distribution family** (Raspberry Pi OS: normally already
  present; DietPi: `apt install polkitd`, absent by default; others: the
  package name), the verification command (`busctl … CanPowerOff` →
  `s "yes"`), and what happens without it (buttons disabled, this section
  referenced from the UI). One new row in the access table: OS power
  actions → polkit rule + logind. Deliberately **one** section rather than
  one per distribution: the existing Raspberry Pi OS / DietPi subsections
  already handle divergence with a short note each, and three parallel
  sections would drift on the first change.
- **`docs/interface.md`** — the two routes, the JSON shape, the `null`
  contract, the three actions, and the fact that `restart-service` relies
  on the unit's `Restart=always` (so it behaves as a plain stop outside
  systemd).

## Testing

**Rust unit** (`system.rs`): every pure parser against fixtures, including
malformed input and absent keys — `parse_temperature`, `parse_khz_to_mhz`,
`parse_loadavg`, `parse_meminfo`, `parse_uptime`, `parse_os_release`,
`parse_alarm`, `parse_can`, `parse_action`. `disk_usage("/")` gets one
smoke test asserting a non-zero total (the suite runs on Linux).

**Rust HTTP** (`status.rs`-style `oneshot` tests): `GET /api/system`
returns 200 with the expected key set and tolerates `null` fields;
`can_power_off`/`can_reboot` reflect `SystemInfo`; `POST` with an unknown
action returns 422 with an `error` string and spawns nothing; `POST
poweroff` with `systemctl` pointed at `/bin/true` returns 202; at
`/bin/false` returns 502 with an `error`; `POST restart-service` returns
202 and fires the injected hook (short `restart_delay`).

**Vitest** (`SystemView.test.ts`, `sparkline.test.ts`): values rendered and
`—` for `null`; the dialog gates the POST (no request before Confirm, none
after Cancel); disabled buttons with the explanation when capabilities are
false; the powering-off state stops polling; the restart flow resumes when
`service_uptime_s` drops and times out after 30 s; the history fills, caps
at 60, and starts empty; `cheminSparkline` for 0, 1, n points, clamping and
inversion.

**E2e**: the nav shows the System tab and `/system` renders the cards, with
a non-empty kernel value. **No power action is ever confirmed in e2e** —
the harness runs a real core on the development machine; confirming
`poweroff` would shut down that machine and `restart-service` would kill
the harness mid-run. Dialog interaction is covered by vitest, which has no
machine to lose.

## Out of scope

No remote-control binding for OS power (one unlucky press cuts everything
off), no authentication (the device has none anywhere), no persisted or
server-side metrics history, no graphs beyond the in-page sparkline, no
metrics on the HDMI display, no per-process or network-throughput metrics,
no fan or CPU-governor control, no change to the existing log panel on the
config page.

## Hardware checks (owner, after merge)

- `under_voltage`: confirm `rpi_volt` exposes `in0_lcrit_alarm` on the
  DietPi kernel — otherwise the card stays silent, which is the designed
  fallback, not a bug.
- `cpu_mhz` and `temperature_c` present on the Pi.
- The polkit rule: power off once from the tab **while logged in over
  SSH**, which is the case that needs `*-multiple-sessions`.
- « Redémarrer Ritornello »: the page recovers on its own within a few
  seconds.

## Amendments (2026-08-14)

This is a living record for this chantier, not an archive: the tab shipped,
the owner tried it, and asked for changes. What follows is what changed
after the design above, folded into the sections it touches (this note is
the index, not a duplicate of the detail):

- **Card order**: CPU, Memory, History, Storage, Device, Power — not
  Load/History/Memory/Storage/Device/Power as originally designed. The load
  average moved from the (renamed) CPU card into the History card, next to
  the series it complements.
- **CPU usage is now a real, computed-in-the-page percentage**
  (`100 × (1 − Δidle / Δtotal)` between two polls' `/proc/stat` jiffy
  counters), replacing `min(100, load[0] / cpus * 100)`. The load average
  moves too slowly to read as a live CPU figure, especially once the
  refresh period below can go down to 1 s. The core exposes the raw
  cumulative counters rather than a ready-made percentage on purpose: a
  percentage needs a remembered previous reading, and that shared state
  would let two browser tabs polling out of phase corrupt each other's
  delta.
- **Refresh period is now user-selectable**: 1, 2, 5, 10, or 30 s,
  defaulting to 5 s, a page-local preference (not persisted) rather than
  the original fixed 5 s. The visible history window is `60 × période`,
  which the header states from the history's actual timestamps rather than
  from that formula (see History graph), since the formula alone is wrong
  for up to 60 polls after any arrival or period change.
- **The undervoltage warning moved from the Power card to the Device
  card**, and split in two: a permanent three-state row (no sensor /
  nominal / undervoltage, short words, red when active) in the Device
  grid, plus the full advisory sentence as a separate `role="status"` red
  line directly under the grid, shown only while the alert is active. The
  original design showed the full sentence as a single Power-card warning,
  visible only when active.
- **Hover / touch readout on the history graph**: not part of the original
  design at all. Pointer events over the sparkline show a vertical line and
  a popin with the pointed sample's time and both percentages; this is also
  why samples now carry a timestamp (`t`), originally omitted since nothing
  consumed it.
