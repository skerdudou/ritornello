<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, Input, Select, SelectContent, SelectItem,
  SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { PERIODS_S, useMetrics } from '../composables/useMetrics'
import type { DateFormat, LogsPayload, SettingsPayload, SystemPayload, SystemUsage } from '../types'
import { lineDate, filterLines } from './log'
import { xValues, sparklinePath, minuteTicks } from './sparkline'

const { t } = useCatalog()
const {
  state, unavailable, history, currentCpuUsage,
  periodMs, period, windowMinutes, pause, resume,
} = useMetrics()

/**
 * The latest logged errors, fetched on mount and every time the popin opens
 * (see `fetchLog`).
 *
 * They used to live on the Configuration page; their place is here, with the
 * metrics, for when you are trying to find out why the device misbehaves.
 *
 * Deliberately outside `probe()`, despite the temptation: the polling holds an
 * "in flight" lock so that a timer faster than the network does not stack two
 * readings, and it computes CPU usage as a delta between two responses.
 * Grafting a second request onto it lengthens how long the lock is held and
 * changes the observed cadence — measured, four cadence tests fell. Refreshing
 * the list would therefore need its own timer, not a free ride.
 */
const logs = ref<string[]>([])

/** Error lines shown directly in the card. Beyond that, the popin takes
 *  over: the core buffer keeps 500 of them, and unrolling them in the page
 *  would push everything else off screen. */
const CARD_LOGS = 8
const errorsOpen = ref(false)
const errorsQuery = ref('')
/**
 * The time-formatting settings, fetched once on mount.
 *
 * One more request on this page, and it is the fair price: the defaults are
 * good enough until it has answered, and a failure leaves the log dated with
 * the defaults rather than depriving it of dates.
 */
const clock = ref<{ date_format: DateFormat; clock_24h: boolean }>({
  date_format: 'day_month_year',
  clock_24h: true,
})

/**
 * The lines rewritten in the configured format, **before** the filter: what
 * you search is what you see, so a search for "14:03" must match the
 * displayed time, not the UTC the core wrote.
 */
const logDates = computed(() =>
  logs.value.map((l) => lineDate(l, clock.value.date_format, clock.value.clock_24h)),
)
const cardLogs = computed(() => logDates.value.slice(0, CARD_LOGS))
const filteredLogs = computed(() => filterLines(logDates.value, errorsQuery.value))

/**
 * Fetches the log: on mount, and every time the popin opens.
 *
 * A user gesture, so always outside the periodic polling — see the comment on
 * `logs`: `probe()` holds an "in flight" lock and computes a CPU delta
 * between two responses, and grafting a second request onto it changes the
 * observed cadence (measured, four cadence tests fell).
 *
 * Its own `.catch`: an unavailable log must not deprive the user of the
 * metrics, nor the other way around. A failure leaves the previous list in
 * place rather than clearing it — same convention as `reload` in
 * `useCatalog`.
 */
async function fetchLog(): Promise<void> {
  const j = await api.get<LogsPayload>('/api/logs').catch(() => null)
  if (j) logs.value = j.lines ?? []
}

function openErrors(): void {
  // Filter reset: a popin that opens shows everything. Keeping the previous
  // query would reopen it on a truncated list, and the field that explains
  // it is at the top of the dialog, not under the eyes of whoever just
  // clicked the button.
  errorsQuery.value = ''
  errorsOpen.value = true
  void fetchLog()
}

/** Chart frame, in `viewBox` units. */
const WIDTH = 100
const HEIGHT = 30
/** Value the machine does not expose: an em dash rather than a 0, which
 *  would read as a measurement. */
const NOTHING = '—'

/** Becomes false on unmount. Only guards `waitForReturn` now: its tight
 *  polling loop stops on the next iteration, and its failure message is no
 *  longer shown once the view has been left. The regular polling, for its
 *  part, lives in the store and no longer depends on this flag — `start()`
 *  does not consult it. */
let mounted = true

/**
 * Label of the trigger, computed here rather than left to `SelectValue`
 * without content: the latter displays the text of the selected option **as
 * captured at mount time**, but the catalog arrives later (shared async
 * loading, see `useCatalog`). The rest of the page fixes itself when it
 * arrives, `t` being a computed — but that particular text stayed frozen on
 * "5 system_unit_second", raw key included. An expression here is re-read on
 * every render, hence immune to that capture.
 */
const periodLabel = computed(
  () => `${periodMs.value / 1000} ${t.value('system_unit_second')}`,
)

// The metrics polling is not started here: it is started once for the whole
// SPA by `App.vue`. All that remains at mount time is the log — outside the
// periodic polling, yet not fetched only once in the view's lifetime either:
// opening the popin fetches it again (see `fetchLog`).
onMounted(() => {
  void fetchLog()
  // Its own `.catch`: unreachable settings must not deprive the page of its
  // metrics or its log.
  void api
    .get<SettingsPayload>('/api/settings')
    .then((r) => {
      clock.value = { date_format: r.date_format, clock_24h: r.clock_24h }
    })
    .catch(() => {})
})
onUnmounted(() => {
  mounted = false
})

// "°C" and "MHz" are not translated: they are SI symbols, identical in both
// languages — unlike Mo/MB and j/d.
const temperature = computed(() =>
  state.value?.temperature_c == null ? NOTHING : `${state.value.temperature_c.toFixed(1)} °C`,
)
const frequency = computed(() =>
  state.value?.cpu_mhz == null ? NOTHING : `${state.value.cpu_mhz} MHz`,
)
const load = computed(() =>
  state.value?.load ? state.value.load.map((v) => v.toFixed(2)).join(' · ') : NOTHING,
)
const usageText = computed(() =>
  currentCpuUsage.value == null ? NOTHING : `${Math.round(currentCpuUsage.value)} %`,
)
/**
 * Threshold at which CPU usage turns into an alert. Strictly greater: an
 * exact 90 % is not yet an alert.
 *
 * Compared against the **displayed** (rounded) value, not the raw one:
 * otherwise 90 < u <= 90.5 would display "90 %" while triggering the alert,
 * which neither the label nor the comment above suggest.
 */
const CPU_ALERT_THRESHOLD = 90
const cpuAlerting = computed(() => Math.round(currentCpuUsage.value ?? 0) > CPU_ALERT_THRESHOLD)
/**
 * Width of the bar. Passed through a computed rather than inline: the
 * template does not have to narrow a `number | null` behind its `v-if`,
 * which the type check does not always follow across that boundary.
 */
const cpuWidth = computed(() => Math.round(currentCpuUsage.value ?? 0))
/**
 * Power-supply line of the Device card, always displayed — never hidden
 * behind a `v-if` — to distinguish four situations the old display
 * conflated: no probe (`null`, rendered "—" like any other missing metric),
 * a probe reporting a healthy power supply with no prior episode, a power
 * supply healthy *right now* but that dropped at least once since boot
 * (`under_voltage_since_boot` — the firmware's sticky bit, distinct from the
 * instantaneous `under_voltage` alarm: an episode lasts from a few
 * milliseconds to a few seconds, which a 5 s poll has very little chance of
 * catching while it happens), and an under-voltage actually detected right
 * now (`under_voltage === true`, which wins over the prior episode — no
 * point saying "seen before" when it is happening again). A permanent line
 * that turns red is as visible as a banner.
 *
 * The word is short ("Under-voltage", not the full sentence): the advice
 * sentence (`system_under_voltage`) lives separately, just below the grid,
 * and only appears when the **instantaneous** alert is active — one single
 * place for the state, one for the advice, rather than both concatenated in
 * a two-column grid cell that made them overflow. The new state, for its
 * part, does not trigger that sentence: it says nothing to do right now,
 * only what already happened — which the help (the `(?)` button below)
 * explains, without repeating the alert.
 */
const voltage = computed(() => {
  if (state.value?.under_voltage == null) return NOTHING
  if (state.value.under_voltage) return t.value('system_voltage_low')
  if (state.value.under_voltage_since_boot) return t.value('system_voltage_since_boot')
  return t.value('system_voltage_ok')
})

/** Open state of the under-voltage help popin (see the `(?)` button in the
 *  template): view-local state, like `dialog` for the power actions, but
 *  deliberately separate — the two popins have nothing in common besides the
 *  kit's `Dialog` component. */
const voltageHelpOpen = ref(false)
const last = computed(() => history.value.at(-1) ?? null)
/**
 * X coordinates shared by everything positioned on the chart: the three
 * paths, the hover line and the popover placement. A single source, so that
 * none of them can drift from the others.
 */
const chartXValues = computed(() =>
  xValues(history.value.map((h) => h.t), WIDTH),
)
const cpuPath = computed(() =>
  sparklinePath(history.value.map((h) => h.cpu), chartXValues.value, HEIGHT),
)
const ramPath = computed(() =>
  sparklinePath(history.value.map((h) => h.ram), chartXValues.value, HEIGHT),
)
/**
 * Temperature path, in °C on the **same 0-100 axis** as the two percentages:
 * a Pi's °C live in that range (throttling at 80-85), so mid-height reads as
 * "50 °C" without a second scale, and `sparklinePath` already clamps to
 * 0-100 — a machine above 100 °C would flatten against the top of the
 * frame, which is the least of its problems. The legend is what carries the
 * unit, and it is what makes a mixed axis honest.
 *
 * A missing value opens a **gap** in the path rather than erasing the whole
 * curve or copying the last known temperature over it — see the contract of
 * `sparklinePath`, which accepts `null` directly for that. The old version
 * erased everything at the slightest missing reading, on the grounds that
 * the three paths, the hover line and the popover share a single set of
 * xValues (`chartXValues`) and that a shorter series would drift from the
 * others; that rationale only held for a *truncated* series (values removed,
 * hence shifted by one rank). A gap, for its part, keeps every present
 * temperature on its own x coordinate — that of its timestamp, exactly as
 * in the two other curves — so nothing drifts. A machine without a probe
 * still has no curve at all (all values are `null`), and a transient gap no
 * longer erases anything but the affected segment, not the twenty minutes
 * or two hours of history around it.
 */
const tempPath = computed(() =>
  sparklinePath(history.value.map((h) => h.temp), chartXValues.value, HEIGHT),
)

/** Height of the minute ticks, in `viewBox` units: a notch on the bottom of
 *  the frame, short enough not to cross the curves. */
const TICK_HEIGHT = 4
/** X coordinates of the minute ticks (see `minuteTicks`). */
const ticks = computed(() =>
  minuteTicks(history.value.map((h) => h.t), WIDTH),
)

/** Index of the hovered column in `history`, `null` when the pointer is not
 *  on the chart. */
const hoverAtIndex = ref<number | null>(null)

/** Width of the chart in pixels, measured on the last pointer event: used
 *  to clamp the popover position in real pixels (see `popoverStyle`) rather
 *  than as a percentage of the container — a pixel clamps directly, a
 *  percentage would require knowing in advance the popover width relative to
 *  the card's, which varies. */
const chartWidth = ref(0)

/**
 * Translates the pointer position into a sample index: the sample whose x
 * coordinate is **closest** to the pointer.
 *
 * The computation can no longer be a simple rank rounding (`frac × (n - 1)`):
 * the points are no longer equidistant since they sit at their timestamp, so
 * a proportional rank no longer designates the column you see under the
 * cursor. The search starts from the same xValues as the path, which
 * guarantees by construction that the popover does not drift from the curve
 * it annotates. A linear loop over at most 240 points, on every
 * `pointermove`: far below any budget.
 */
function hoverIndex(event: PointerEvent): number {
  const rect = (event.currentTarget as Element).getBoundingClientRect()
  chartWidth.value = rect.width
  const frac = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0
  const target = Math.min(1, Math.max(0, frac)) * WIDTH
  let closest = 0
  let bestDistance = Number.POSITIVE_INFINITY
  // `<=` and not `<`: on an exact tie — pointer exactly halfway between two
  // columns — the right-hand column wins. This tie-break is not an
  // implementation detail but the behavior the rounding test already pinned,
  // `Math.round` rounding halves upward. Silently changing it while moving
  // from rank to x coordinate would have been a regression invisible to the
  // eye.
  //
  // `forEach` rather than an indexed loop: it hands over the x coordinate
  // itself, where an `xs[i]` would require handling an `undefined` that the
  // array length already rules out.
  chartXValues.value.forEach((x, i) => {
    const distance = Math.abs(x - target)
    if (distance <= bestDistance) {
      bestDistance = distance
      closest = i
    }
  })
  return closest
}

/**
 * `pointermove` and `pointerdown` share this handler: the former covers both
 * mouse hover and touch drag, the latter shows the popover as soon as a
 * touch screen is pressed (without it, a plain tap with no movement would
 * never trigger `pointermove`).
 */
function hoverPointer(event: PointerEvent) {
  if (history.value.length < 2) return
  hoverAtIndex.value = hoverIndex(event)
}

/**
 * Clears the popover. `pointerleave` and `pointercancel` are enough to cover
 * the pointer leaving, whether mouse or finger: the pointer events
 * specification already fires `pointerout` then `pointerleave` right after
 * the `pointerup` of a direct-manipulation pointer (the finger lifting). A
 * separate `@pointerup` here therefore added nothing — and made things
 * worse: on a touch screen, a plain tap showed then cleared the popover in
 * under 100 ms (only a press-and-hold or a drag left time to read it), and
 * with a mouse, clicking the chart hid it until the next movement.
 */
function endHover() {
  hoverAtIndex.value = null
}

/** X coordinate of the hover line, in `viewBox` units: that of the hovered
 *  sample, read from `chartXValues` and not recomputed from its rank — that
 *  is what keeps it exactly on the curve. */
const hoverLineX = computed(() => {
  const i = hoverAtIndex.value
  if (i === null || history.value.length < 2) return null
  return chartXValues.value[i] ?? null
})

/** Hovered sample, for the three values displayed in the popover (the
 *  temperature only appears there if the machine exposes one). */
const hoveredSample = computed(() => {
  if (hoverAtIndex.value === null) return null
  return history.value[hoverAtIndex.value] ?? null
})

/** Fixed width of the popover (see the `min-w-` class on its element): used
 *  to know its half extent to clamp it below, without depending on the
 *  displayed text. */
const POPOVER_WIDTH_PX = 100
const HALF_POPOVER_WIDTH_PX = POPOVER_WIDTH_PX / 2

/**
 * Horizontal position of the popover: always centered on the hovered column
 * (constant -50 % translation), with the position clamped in pixels rather
 * than clamping the hovered column itself.
 *
 * The old code only clamped the two extreme columns (`i === 0` and
 * `i === n - 1`) by disabling centering on them alone — reasoning designed
 * for two overflowing columns, when the overflow actually affects a whole
 * band of columns near the edges (all those within half a popover of the
 * card edge), not just the last two. With a full buffer (240 samples) in a
 * narrow card, that let the popovers of indexes 1 through roughly 4
 * overflow, and symmetrically at the end of the series — precisely what the
 * clamp exists to prevent.
 *
 * Clamping in pixels (`chartWidth`, measured on the last pointer event) and
 * not via a CSS `clamp()` mixing `%` and `calc()`: both would render exactly
 * the same thing in a browser, but a pixel clamps with a plain
 * `Math.min`/`Math.max`, without depending on a CSS engine to interpret it —
 * which includes the very limited one of the test environment.
 */
const popoverStyle = computed(() => {
  const n = history.value.length
  const i = hoverAtIndex.value
  if (i === null || n < 2) return null
  // Fraction read from the shared xValues, not `i / (n - 1)`: the columns
  // are no longer equidistant, and a popover pinned to the rank would shift
  // away from the column it annotates as soon as the polling period changes
  // along the way.
  const fraction = (chartXValues.value[i] ?? 0) / WIDTH
  const width = chartWidth.value
  if (width <= 0) {
    // Width not measured yet: unclamped fallback rather than a division by
    // zero — a case that should not occur in practice, since the pointer
    // event that produces `i` has already measured that width on the way.
    return { left: `${fraction * 100}%`, transform: 'translateX(-50%)' }
  }
  const center = fraction * width
  const upperBound = Math.max(width - HALF_POPOVER_WIDTH_PX, HALF_POPOVER_WIDTH_PX)
  const left = Math.min(Math.max(center, HALF_POPOVER_WIDTH_PX), upperBound)
  return { left: `${left}px`, transform: 'translateX(-50%)' }
})

function text(v: string | null | undefined): string {
  return v || NOTHING
}

function number(v: number | null | undefined): string {
  return v == null ? NOTHING : String(v)
}

/** "512 / 976 MB": used and total in the same unit, translated. */
function usage(u: SystemUsage | null | undefined, unit: 'mb' | 'gb'): string {
  if (!u) return NOTHING
  const divisor = unit === 'mb' ? 1024 : 1024 * 1024
  const format = (kb: number) =>
    unit === 'mb' ? String(Math.round(kb / divisor)) : (kb / divisor).toFixed(1)
  const suffix = t.value(unit === 'mb' ? 'system_unit_mb' : 'system_unit_gb')
  return `${format(u.total_kb - u.available_kb)} / ${format(u.total_kb)} ${suffix}`
}

function usedPercent(u: SystemUsage | null | undefined): number {
  if (!u || u.total_kb === 0) return 0
  return Math.round(((u.total_kb - u.available_kb) / u.total_kb) * 100)
}

/** At most two units: "3 d 4 h", "4 h 12 min", "12 min". */
function duration(seconds: number | null | undefined): string {
  if (seconds == null) return NOTHING
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const day = t.value('system_unit_day')
  const hour = t.value('system_unit_hour')
  const minute = t.value('system_unit_minute')
  if (d > 0) return `${d} ${day} ${h} ${hour}`
  if (h > 0) return `${h} ${hour} ${m} ${minute}`
  return `${m} ${minute}`
}

type PowerAction = 'poweroff' | 'reboot' | 'restart-service'

/** Tight polling while waiting for the return, whatever the action. */
const RESUME_MS = 2000
/** Waiting cap for the **service** restart: systemd restarts the process
 *  within a second (`Restart=always`), 30 s comfortably cover a slow
 *  startup. */
const MAX_RESUME_MS = 30000
/** Waiting cap for a **machine** reboot: four times as much, because a Pi
 *  does not come back like a process — services stopping, kernel boot,
 *  mounts, network, and only then the service. On the order of 20 to 40 s on
 *  healthy hardware (not measured here); 120 s leave margin for a slow SD
 *  card or an incidental `fsck`, without leaving the user in front of a
 *  message that never concludes. */
const MAX_RESUME_REBOOT_MS = 120_000

/** Action awaiting confirmation, and action in progress. */
const dialog = ref<PowerAction | null>(null)
const inProgress = ref<PowerAction | null>(null)

function label(a: PowerAction): string {
  if (a === 'poweroff') return t.value('system_poweroff')
  if (a === 'reboot') return t.value('system_reboot')
  return t.value('system_restart_service')
}

function consequence(a: PowerAction): string {
  if (a === 'poweroff') return t.value('system_confirm_poweroff')
  if (a === 'reboot') return t.value('system_confirm_reboot')
  return t.value('system_confirm_restart_service')
}

const currentMessage = computed(() => {
  if (inProgress.value === 'poweroff') return t.value('system_powering_off')
  if (inProgress.value === 'reboot') return t.value('system_rebooting')
  if (inProgress.value === 'restart-service') return t.value('system_restarting')
  return ''
})

/** The confirmation button is only painted "destructive" for the actions
 *  that really are: restarting the service leaves the device powered on,
 *  which its own consequence sentence promises. */
const confirmVariant = computed(() => (dialog.value === 'restart-service' ? 'default' : 'destructive'))

/**
 * The core is about to disappear: the normal polling stops before the send.
 * Without that, the next poll would fail and display an alarming network
 * error while the shutdown goes exactly as requested.
 *
 * Two of the three actions then wait for the return, and only one stays
 * suspended: poweroff. A machine reboot is awaited like a service restart —
 * longer, see `MAX_RESUME_REBOOT_MS` — because the device comes back while
 * the tab, for its part, stayed open. Leaving the polling paused would
 * freeze the chart of **every** page until a full reload, with nothing on
 * screen to explain it: `inProgress` is local to the view and disappears
 * with it, `unavailable` stays false. Only poweroff justifies the permanent
 * suspension, the device coming back only through a physical gesture.
 */
async function confirm() {
  const action = dialog.value
  if (!action) return
  dialog.value = null
  inProgress.value = action
  pause()
  const uptimeBefore = state.value?.service_uptime_s ?? null
  const err = await api.post('/api/system/power', { action })
  if (err) {
    // Refusal from logind (missing polkit rule) or unreachable core: nothing
    // is shutting down, hand control back. An ordinary path on this machine,
    // not an edge case — a DietPi install without the polkit rule, or with
    // `systemd-logind` masked, refuses the very first call.
    toast.error(err)
    inProgress.value = null
    resume()
    return
  }
  if (action === 'restart-service') {
    await waitForReturn(uptimeBefore, MAX_RESUME_MS, 'system_restarted')
  } else if (action === 'reboot') {
    await waitForReturn(uptimeBefore, MAX_RESUME_REBOOT_MS, 'system_device_restarted')
  }
}

/**
 * The service — or the whole machine — is restarting: poll faster while
 * ignoring errors (it is down, that is expected). The cap and the success
 * message come as parameters rather than being derived from the action here:
 * the function does not have to know the page's three actions, and a cap
 * named at the call site reads together with the reason that motivates it.
 *
 * It is only considered back once its uptime is *lower than what the old
 * process would display by now* — not merely lower than `before`: right
 * after a successful restart, `service_uptime_s` is very often 0, and
 * nothing can ever be strictly lower than 0. Comparing against
 * `before + elapsed` (the uptime the old process, for its part, keeps
 * accumulating while we wait) stays true even when the returned process
 * displays 0. No margin added to this threshold: `Math.floor` can only delay
 * acceptance by one second, whereas a margin added to the threshold would
 * make acceptance easier and could pass the *old* process off as a restarted
 * one — exactly the bug this uptime comparison exists to prevent.
 *
 * The same test holds for a machine reboot, and a reader might doubt it: it
 * really is `service_uptime_s` being compared, not `uptime_s`, and it starts
 * over from zero with the machine since the service restarts along with it.
 * A full reboot therefore satisfies the threshold at least as clearly as a
 * mere service restart — there is nothing to adapt.
 *
 * `mounted` in the loop condition: if the user has left the view, this tight
 * polling stops on the next iteration rather than running to the cap for a
 * page nobody is looking at anymore. What follows the loop, however, must
 * run in both cases: the regular polling lives in the store, shared by the
 * whole SPA, and leaving it paused would freeze the chart of every page
 * until a full reload. Resuming on an unmounted view is no longer the danger
 * this comment used to fear — the timer outlives every view anyway, that is
 * its reason for being. Only the failure message stays gated on `mounted`.
 */
async function waitForReturn(before: number | null, maxMs: number, successKey: string) {
  const t0 = Date.now()
  const deadline = t0 + maxMs
  while (mounted && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, RESUME_MS))
    try {
      // The poll is raced against a delay: without it, a request that
      // connects but never answers (flaky Wi-Fi, half-open socket) would
      // block the wait here, indefinitely, beyond the cap promised to the
      // user. The abandoned request stays in flight but has no further
      // effect: the loop has already moved on.
      const s = await Promise.race([
        api.get<SystemPayload>('/api/system'),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error('probe without response')), RESUME_MS),
        ),
      ])
      const elapsed = Math.floor((Date.now() - t0) / 1000)
      if (before === null || s.service_uptime_s < before + elapsed) {
        state.value = s
        inProgress.value = null
        // No guard on `mounted` here, unlike the timeout message below: this
        // is deliberate. A success announced after the user left the view is
        // still useful information; a failure reported far too late is only
        // noise. Do not "fix" this asymmetry into symmetry.
        toast.success(t.value(successKey))
        resume()
        return
      }
    } catch {
      // Service down, or probe without response: retry until the cap.
    }
  }
  // Exit by cap **or** by unmount: in both cases the polling must resume. It
  // is now shared by the whole SPA, and a bare `return` on `!mounted` would
  // leave it paused for good — every page's chart frozen, with nothing on
  // screen to explain it.
  //
  // A trade-off seen and accepted, not overlooked: this resume is
  // unconditional, so a loop left over from an unmounted instance can, on
  // waking from its `RESUME_MS` sleep, resume a suspension that a *just*
  // confirmed power action had taken. The window is bounded to 2 s and the
  // case requires leaving the view during a wait then reconfirming right
  // away; the clean remedy is a suspension token rather than a boolean, and
  // it is out of scope here. Between that risk and a `paused` frozen for the
  // life of the page, this is the one we take.
  //
  // Making `reboot` wait on `waitForReturn` widens this risk on two fronts,
  // not one: before, only the service restart went through this loop, so
  // only reconfirming a service restart during its wait could trigger it;
  // the machine reboot opens a second trigger. And the period during which
  // leaving the view can spawn such a loop stretches with its cap: at most
  // 30 s previously (`MAX_RESUME_MS`), at most 120 s now
  // (`MAX_RESUME_REBOOT_MS`) for a reboot confirmed then abandoned.
  inProgress.value = null
  resume()
  // The failure message, for its part, stays conditional: a failure reported
  // one or two minutes after the user left the view is only noise.
  if (!mounted) return
  toast.error(t.value('system_restart_timeout'))
}
</script>

<template>
  <div class="space-y-4">
    <p v-if="unavailable" data-system-unavailable class="text-sm text-destructive">
      {{ t('system_unavailable') }}
    </p>

    <!-- No CardTitle associated with the trigger here (no Card around it):
         aria-label required, same rationale as the Selects in ConfigView.vue. -->
    <div class="flex items-center gap-2">
      <span class="text-sm text-muted-foreground">{{ t('system_period') }}</span>
      <Select v-model="period">
        <SelectTrigger data-system-period class="w-24" :aria-label="t('system_period')">
          <SelectValue>{{ periodLabel }}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem v-for="p in PERIODS_S" :key="p" :value="String(p)">
            {{ p }} {{ t('system_unit_second') }}
          </SelectItem>
        </SelectContent>
      </Select>
    </div>

    <Card>
      <CardHeader><CardTitle>{{ t('system_cpu') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div class="grid gap-2 sm:grid-cols-2">
          <div>{{ t('system_temperature') }} : <span data-system-temperature>{{ temperature }}</span></div>
          <div>{{ t('system_frequency') }} : <span data-system-frequency>{{ frequency }}</span></div>
          <div>{{ t('system_cores') }} : <span data-system-cores>{{ number(state?.cpus) }}</span></div>
        </div>
        <!-- Usage leaves the grid of the three other metrics to hold its own
             line, right above its bar: that is what labels it. In the grid it
             landed in the second column, next to the core count, and the
             full-width bar below no longer announced what it measured. Same
             shape as Memory and Storage: a line of text, then its bar. -->
        <div>
          {{ t('system_cpu_usage') }} :
          <span data-system-cpu-usage :class="cpuAlerting ? 'font-medium text-destructive' : undefined">
            {{ usageText }}
          </span>
        </div>
        <!-- Bar always present, at zero while the percentage is unknown: it
             otherwise appeared all at once on the second poll, pushing the
             layout around. The risk of reading "0 %" into an empty bar is
             covered by the line above, which displays "—" and not "0 %"
             until a delta is computable. -->
        <div data-system-cpu-bar class="h-2 w-full rounded bg-muted">
          <div
            class="h-2 rounded"
            :class="cpuAlerting ? 'bg-destructive' : 'bg-primary'"
            :style="{ width: `${cpuWidth}%` }"
          />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_memory') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-memory>
          {{ usage(state?.memory, 'mb') }}
          <span v-if="state?.memory" class="text-muted-foreground">({{ usedPercent(state.memory) }} %)</span>
        </div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${usedPercent(state?.memory)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle class="flex items-baseline gap-2">
          {{ t('system_history') }}
          <span data-system-history-span class="text-xs font-normal text-muted-foreground">
            {{ t('system_history_span', { minutes: windowMinutes }) }}
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <!-- The chart is **always** rendered, empty until there are two
             samples. A waiting message in its place made the layout jump on
             the second poll, the text suddenly giving way to a 96 px figure.
             No trick needed to get this: `sparklinePath` renders an empty
             string below two points, and an empty `d` is an invisible
             `<path>` — it is written in its contract.

             `relative`: anchors the hover popover to the chart, not to the
             whole card. -->
        <div class="relative">
          <!-- `preserveAspectRatio="none"` stretches the frame to the
               available width; `vector-effect` keeps the stroke width from
               being stretched with it. *Pointer* events, not *mouse*: the
               page is mostly used with a finger, and `pointermove` alone
               already covers mouse hover and touch drag. No
               `touch-action: none` here: it would block vertical page
               scrolling over the chart on a phone. -->
          <svg
            data-system-history
            :viewBox="`0 0 ${WIDTH} ${HEIGHT}`"
            preserveAspectRatio="none"
            class="h-24 w-full"
            role="img"
            :aria-label="t('system_history')"
            @pointermove="hoverPointer"
            @pointerdown="hoverPointer"
            @pointerleave="endHover"
            @pointercancel="endHover"
          >
            <!-- Minute ticks, drawn **before** the curves so they sit
                 underneath: they are landmarks, not data. A notch on the
                 bottom of the frame, with no text — the exact scale is
                 announced once and for all by the card's label, and the
                 value at a precise instant is read on hover. -->
            <line
              v-for="(x, i) in ticks"
              :key="`tick-${i}`"
              data-system-history-tick
              :x1="x"
              :x2="x"
              :y1="HEIGHT - TICK_HEIGHT"
              :y2="HEIGHT"
              class="text-muted-foreground/60"
              stroke="currentColor"
              stroke-width="1"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="cpuPath"
              class="text-primary"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="ramPath"
              class="text-muted-foreground"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <!-- Third curve distinguished by color alone, no dashes:
                 `destructive` is the only hue guaranteed distinct from
                 `primary` and `muted-foreground` across the kit's 42
                 presets. It does not signal an alert here — it is a series
                 color, and the legend says which one. -->
            <path
              data-system-history-temp
              :d="tempPath"
              class="text-destructive"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <!-- Hover line only, no per-series dot: a `<circle>` in a
                 viewBox stretched by `preserveAspectRatio="none"` would draw
                 as an ellipse, not a circle. The line plus the popover
                 values meet the need without that flaw — do not "fix" this
                 by adding circles. -->
            <line
              v-if="hoverLineX !== null"
              data-system-history-line
              :x1="hoverLineX"
              :x2="hoverLineX"
              y1="0"
              :y2="HEIGHT"
              class="text-muted-foreground"
              stroke="currentColor"
              stroke-width="1"
              vector-effect="non-scaling-stroke"
            />
          </svg>
          <!-- `pointer-events-none`: the popover follows the pointer without
               ever getting in its way, otherwise it would capture the very
               events it depends on. -->
          <div
            v-if="hoveredSample && popoverStyle"
            data-system-history-popin
            class="pointer-events-none absolute top-0 min-w-[100px] rounded-md border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-md"
            :style="popoverStyle"
          >
            <div>{{ new Date(hoveredSample.t).toLocaleTimeString() }}</div>
            <div class="text-primary">{{ t('system_cpu') }} {{ Math.round(hoveredSample.cpu) }} %</div>
            <div class="text-muted-foreground">{{ t('system_memory') }} {{ Math.round(hoveredSample.ram) }} %</div>
            <div v-if="hoveredSample.temp !== null" class="text-destructive">
              {{ t('system_temperature') }} {{ hoveredSample.temp.toFixed(1) }} °C
            </div>
          </div>
        </div>
        <!-- `—` and not "0 %" without a sample: same convention as the CPU
             reading above, so as not to announce a measurement we do not
             have yet. -->
        <p data-system-history-legend class="mt-2 flex gap-4 text-xs">
          <span class="text-primary">
            {{ t('system_cpu') }} {{ last ? `${Math.round(last.cpu)} %` : NOTHING }}
          </span>
          <span class="text-muted-foreground">
            {{ t('system_memory') }} {{ last ? `${Math.round(last.ram)} %` : NOTHING }}
          </span>
          <!-- Announced from `state` and not from the last sample: whether a
               probe exists is known from the first poll, so the legend does
               not gain a column along the way. The value itself does come
               from the sample, like the two others. -->
          <span v-if="state?.temperature_c != null" class="text-destructive">
            {{ t('system_temperature') }}
            {{ last?.temp != null ? `${last.temp.toFixed(1)} °C` : NOTHING }}
          </span>
        </p>
        <!-- Available from the first poll, unlike the CPU delta: a
             time-averaged figure does not need two measurements. -->
        <p class="mt-2 text-xs text-muted-foreground">
          {{ t('system_loadavg') }} : <span data-system-load>{{ load }}</span>
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_storage') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-disk>{{ usage(state?.disk, 'gb') }}</div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${usedPercent(state?.disk)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_device') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div class="grid gap-2 sm:grid-cols-2">
          <div>{{ t('system_hostname') }} : <span data-system-hostname>{{ text(state?.hostname) }}</span></div>
          <div>{{ t('system_ip') }} : <span data-system-ip>{{ text(state?.ip) }}</span></div>
          <div>{{ t('system_os') }} : <span data-system-os>{{ text(state?.os) }}</span></div>
          <div>{{ t('system_kernel') }} : <span data-system-kernel>{{ text(state?.kernel) }}</span></div>
          <div>{{ t('system_version') }} : <span data-system-version>{{ text(state?.version) }}</span></div>
          <!-- The voltage moves up here, opposite the version, so that the
               two uptimes end up side by side on the next line: they are the
               ones read together ("the machine has been up for X, the
               service for Y"), and the two-column grid kept them apart. -->
          <div>
            {{ t('system_voltage') }} :
            <span data-system-under-voltage :class="{ 'text-destructive': state?.under_voltage === true }">
              {{ voltage }}
            </span>
            <!-- A help button, not text unfolded here: this cell lives in
                 the two-column grid the advice sentence
                 (`system_under_voltage`, below the grid) had precisely been
                 **moved out of** during the system work, because a long text
                 overflowed its cell there. The help is even longer than that
                 advice, so it has even less of a place here — hence the
                 popin rather than an in-place paragraph.
                 `size="icon-xs"`: small enough to stay a mere "(?)" next to
                 the label, not a button competing with it. -->
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              data-system-voltage-help
              :aria-label="t('system_voltage_help')"
              @click="voltageHelpOpen = true"
            >
              ?
            </Button>
          </div>
          <div>{{ t('system_uptime') }} : <span data-system-uptime>{{ duration(state?.uptime_s) }}</span></div>
          <div>
            {{ t('system_service_uptime') }} :
            <span data-system-service-uptime>{{ duration(state?.service_uptime_s) }}</span>
          </div>
        </div>
        <!-- One single place for the state (the line above, short:
             "Under-voltage" or "Nominal"), one for the advice that goes with
             it — and that advice only exists when it applies. Before, the
             full sentence lived in the grid itself: doubled colons ("Supply
             voltage: Under-voltage detected: check the power supply.") and a
             text overflowing its two-column cell. -->
        <p
          v-if="state?.under_voltage === true"
          data-system-under-voltage-avis
          role="status"
          class="text-destructive"
        >
          {{ t('system_under_voltage') }}
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_power') }}</CardTitle></CardHeader>
      <CardContent class="space-y-3">
        <p v-if="inProgress" data-power-progress aria-live="polite" class="text-sm text-muted-foreground">
          {{ currentMessage }}
        </p>
        <p
          v-else-if="state && (!state.can_power_off || !state.can_reboot)"
          data-power-unavailable
          class="text-sm text-muted-foreground"
        >
          {{ state.logind_reachable ? t('system_power_unavailable') : t('system_power_no_logind') }}
        </p>
        <div class="flex flex-wrap gap-2">
          <Button
            variant="destructive"
            data-power-poweroff
            :disabled="!!inProgress || !state?.can_power_off"
            @click="dialog = 'poweroff'"
          >
            {{ t('system_poweroff') }}
          </Button>
          <Button
            variant="destructive"
            data-power-reboot
            :disabled="!!inProgress || !state?.can_reboot"
            @click="dialog = 'reboot'"
          >
            {{ t('system_reboot') }}
          </Button>
          <Button
            variant="outline"
            data-power-restart
            :disabled="!!inProgress"
            @click="dialog = 'restart-service'"
          >
            {{ t('system_restart_service') }}
          </Button>
        </div>
      </CardContent>
    </Card>

    <!-- `data-logs-card` and not only `data-log-line`: the card must be
         locatable even when the log is empty, otherwise the end-to-end
         journey could not tell "no errors" from "card gone". -->
    <Card data-logs-card>
      <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2">
        <ul class="space-y-1 font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in cardLogs" :key="i" data-log-line>{{ l }}</li>
        </ul>
        <!-- Offered from the first error on, and not only once the card
             overflows. Reported from real use: reserved for a long log, the
             filter was only discovered at the very moment there is too much
             to read to explore the screen. It only disappears on an empty
             log, where there would be nothing to open. -->
        <Button
          v-if="logs.length"
          variant="outline"
          size="sm"
          data-logs-all
          @click="openErrors"
        >
          {{ t('system_errors_all', { count: logs.length }) }}
        </Button>
      </CardContent>
    </Card>

    <!-- Errors popin: the kit's `Dialog`, like the under-voltage help and
         the power dialog, and rendered like them in a portal — its content
         therefore lives in `document.body`, which the tests know. The
         counter sits in the `DialogDescription`: it does describe the
         dialog, and putting it there gives it its accessibility text for
         free. -->
    <Dialog v-model:open="errorsOpen">
      <!-- Much wider than the other popins, and this is the only case that
           justifies it: those carry a sentence, this one carries log lines.
           The kit's `DialogContent` caps at `sm:max-w-lg` (512 px), where a
           log line wraps three or four times and becomes unreadable. Here we
           take the screen: 95 % of the window, capped at 1920 px so a very
           wide screen does not spread one line across two meters.

           Wider than the page's own `max-w-5xl`, then, and deliberately so:
           the page is a document you read, this dialog is a diagnostic tool
           you scrutinize. -->
      <DialogContent class="sm:max-w-[min(95vw,120rem)]">
        <DialogHeader>
          <DialogTitle>{{ t('system_errors_title') }}</DialogTitle>
          <DialogDescription data-logs-count>
            {{ filteredLogs.length }} / {{ logs.length }}
          </DialogDescription>
        </DialogHeader>
        <Input
          v-model="errorsQuery"
          data-logs-filter
          :placeholder="t('system_errors_filter')"
        />
        <!-- `whitespace-pre-wrap`: a log line aligns its fields with spaces,
             which default HTML rendering collapses to one — the level column
             and the target column ended up shifted from one line to the
             next. Wrapping stays allowed (`pre-wrap` and not `pre`): a long
             line must stay readable without horizontal scrolling.

             70vh rather than 60: the dialog is the only place that shows
             more than the last few lines, so let it show as many as it
             can. -->
        <ul
          class="max-h-[70vh] space-y-1 overflow-y-auto font-mono text-xs whitespace-pre-wrap text-muted-foreground"
        >
          <li v-for="(l, i) in filteredLogs" :key="i" data-logs-dialog-line>{{ l }}</li>
        </ul>
        <p v-if="!filteredLogs.length" data-logs-empty class="text-sm text-muted-foreground">
          {{ t('system_errors_none') }}
        </p>
      </DialogContent>
    </Dialog>

    <!-- Under-voltage help popin, independent of the power dialog below:
         same kit components (`Dialog` already handles focus and escape), no
         shared state or content. -->
    <Dialog v-model:open="voltageHelpOpen">
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ t('system_voltage_help_title') }}</DialogTitle>
          <DialogDescription>{{ t('system_voltage_help_body') }}</DialogDescription>
        </DialogHeader>
      </DialogContent>
    </Dialog>

    <!-- A single dialog for the three actions: the title and the consequence
         sentence come from the pending action. -->
    <Dialog
      :open="dialog !== null"
      @update:open="(open: boolean) => { if (!open) dialog = null }"
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ dialog ? label(dialog) : '' }}</DialogTitle>
          <DialogDescription>{{ dialog ? consequence(dialog) : '' }}</DialogDescription>
        </DialogHeader>
        <div class="flex justify-end gap-2">
          <Button variant="outline" data-power-cancel @click="dialog = null">
            {{ t('system_cancel') }}
          </Button>
          <Button :variant="confirmVariant" data-power-confirm @click="confirm">
            {{ t('system_confirm') }}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  </div>
</template>
