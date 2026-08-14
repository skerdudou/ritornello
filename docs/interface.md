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

The web grid mirrors the same decade window **locally**, through two `<`/`>`
arrows next to the count (shown once it exceeds nine) instead of a `+10`
button: page 0 is 1-9, page k is 10k to 10k+9 — the same boundaries as the
core's offset, so both interfaces agree on what "the same page" means.
Unlike the core, the web grid does **not** wrap: `<` is disabled on the
first page and `>` on the last, and there is no auto-return to page 0. The
physical remote wraps because it has a single key and no way back, so
wrapping is its only way to reach everything; a pair of arrows has a way
back, so wrapping would just be gratuitous and confusing. Picking a preset
no longer changes page either — trying several presets from the same group
should not require paging back each time. The browser always sends the
absolute number to `Select`; only a source/count change resets the page to
0 (the same guard that already resets the window on a source switch).

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

## System page

`GET /api/system` reports OS metrics. **Every metric is optional and is
`null` when the machine does not expose it** — no thermal zone under WSL, no
cpufreq in most VMs, no `rpi_volt` sensor outside a Raspberry Pi — while the
set of keys stays stable:

```json
{
  "temperature_c": 47.8, "cpu_mhz": 900, "load": [0.12, 0.15, 0.09], "cpus": 4,
  "memory": { "total_kb": 948000, "available_kb": 512000 },
  "disk": { "total_kb": 30000000, "available_kb": 24000000 },
  "under_voltage": false, "uptime_s": 84213, "service_uptime_s": 3600,
  "hostname": "ritornello", "ip": "192.168.1.20",
  "os": "Debian GNU/Linux 12 (bookworm)", "kernel": "6.6.51+rpt-rpi-v7",
  "version": "0.1.0", "can_power_off": true, "can_reboot": true,
  "cpu_total_jiffies": 9880976, "cpu_idle_jiffies": 9877777
}
```

Sources: `/sys/class/thermal/thermal_zone0/temp`,
`cpu0/cpufreq/scaling_cur_freq`, `/proc/loadavg`, `/proc/meminfo`
(`MemAvailable`, not `MemFree`), `statvfs("/")` (`f_bavail`, so the blocks
reserved for root are not counted as free), the `rpi_volt` hwmon
`in0_lcrit_alarm`, `/proc/uptime`, `/proc/sys/kernel/{hostname,osrelease}`,
`/etc/os-release`, `/proc/stat`'s aggregate `cpu ` line. The IP address is
the local end of a UDP socket *connected* to a routable address: no packet
is sent and no internet access is needed — the kernel is merely asked
which interface faces the default route.

`cpu_total_jiffies` and `cpu_idle_jiffies` are cumulative counters since
boot, not a percentage: a percentage is a delta, so the page differences
its own successive polls rather than the core remembering a previous
reading — shared state that two browser tabs polling out of phase would
corrupt. `cpu_idle_jiffies` is `idle + iowait`, because `iowait` is time
spent waiting on a disk, not doing work, the same way `top` treats it.

`can_power_off` and `can_reboot` answer logind's `CanPowerOff`/`CanReboot`,
asked **once at startup** and cached: the page polls, and spawning `busctl`
twice per poll would be absurd. Installing the polkit rule therefore takes
effect at the next service start (see
[installation.md](installation.md#shutdown-and-reboot-from-the-web-ui)).

`POST /api/system/power` takes `{"action": "poweroff" | "reboot" |
"restart-service"}`. An unknown action is refused with `422` and an `error`
message. `poweroff` and `reboot` run `systemctl` and wait up to 5 s: `202`
when it succeeds or is still running (the machine is going away); `502`
when it refuses, carrying **logind's own message** whenever it wrote one —
that sentence names the missing polkit rule, which a silent `202` would
hide — or `systemctl a échoué (code N)` when stderr was empty; `500` when
`systemctl` could not be started at all.
`restart-service` answers `202` and exits the process 300 ms later; systemd
restarts it because the unit says `Restart=always`. It needs no privilege,
which is why there is no `can_restart_service` field. Outside systemd, that
action stops the process for good.

The page polls `GET /api/system` while it is open and visible, rather than
receiving a stream: unlike the player state, which the core produces
anyway, these metrics exist only because someone asked for them. The
refresh period is chosen on the page itself — 1, 2, 5, 10, or 30 s,
defaulting to 5 s — and is not persisted: it resets to 5 s on every
arrival, like the history below. The CPU/RAM history graph lives in the
page only — 60 samples at the chosen period (5 minutes at the default
5 s, 1 minute at 1 s, 30 minutes at 30 s), lost on navigation and never
stored.

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
