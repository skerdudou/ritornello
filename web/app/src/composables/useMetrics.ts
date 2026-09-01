import { api } from '@ritornello/ui'
import { computed, ref } from 'vue'
import type { SystemPayload } from '../types'

/**
 * Probing of the system metrics and their history, at **module** level — a
 * single set of state for the whole SPA, like the catalog of `useCatalog`.
 *
 * This is not an implementation detail but the reason this file exists: when
 * this state lived in `SystemView.vue`, leaving the page for the settings and
 * coming back started over from an empty graph, and the history only began to
 * fill up on the first visit. Here, `App.vue` starts it once when the SPA is
 * mounted and it lives until the page is closed.
 *
 * Hence a single bootstrap point: two callers that both started it would
 * fight over the same timer.
 */

/**
 * One history point: two percentages, a temperature in °C when the machine
 * exposes one, and the timestamp of the probe (which carries the x axis, see
 * `xValues` in `views/sparkline.ts`).
 *
 * `temp` is nullable where `cpu` and `ram` are not, and that difference is
 * what matters: a machine without a sensor keeps its graph, whereas a machine
 * whose memory or CPU usage is unreadable has no sample at all.
 */
export interface Sample { cpu: number; ram: number; temp: number | null; t: number }

const state = ref<SystemPayload | null>(null)
const unavailable = ref(false)

/**
 * Probing period, at module level like all the state of this file: it thus
 * lives as long as the page, and a choice made on the System tab is still
 * there when coming back to it — the opposite of the view-local version, which
 * went back to 5 s on every arrival. It is not persisted for all that, neither
 * in `localStorage` nor in `/api/settings`: it is a visualisation comfort, not
 * a device setting, and this SPA keeps no state on the browser side — its
 * preferences live in the core. A full reload therefore brings it back to
 * 5 s. The `Select` only carries strings (see `period` below); the real value
 * in milliseconds lives here for `setInterval`.
 */
const periodMs = ref(5000)
/** Options of the period selector, in seconds. */
export const PERIODS_S = [1, 2, 5, 10, 30] as const
/**
 * Number of samples kept — see `windowMinutes` for the visible window that
 * follows from it at the current period.
 *
 * 240 and not 60: probing now runs continuously, including with the tab hidden
 * and the view unmounted, and a 5-minute memory at the default period would
 * render almost nothing of that continuity. 240 samples make 20 minutes at
 * 5 s, two hours at 30 s. The cap is one of legibility, not of cost: 240
 * points on a graph a few hundred pixels wide are still distinguishable, a
 * few thousand would no longer be.
 */
const CAPACITY = 240

const history = ref<Sample[]>([])
/**
 * In-flight probe, with a dual use: `probe()` uses it as a lock to refuse to
 * overlap itself, `stop()` to cancel it. Before the stateful CPU delta, a late
 * response was only a stale display; now a response landing out of order
 * would overwrite `previousJiffies` with a reference that is too recent or
 * too old, and skew the delta of the next probe (`Δtotal <= 0` or a window
 * much longer than the displayed period). Hence the lock: a probe already in
 * flight blocks a second one rather than letting two responses overtake each
 * other out of order.
 */
let probeInFlight: AbortController | null = null
let timer: ReturnType<typeof setInterval> | null = null
/**
 * One-shot wait before resuming the rhythm, when `start()` finds that the
 * deadline of the current period has not been reached yet. Distinct from
 * `timer` because it only ticks once, and stopped by `stop()` like it — a
 * forgotten `setTimeout` would relight probing in the middle of a power
 * action, or double the timer after a period change. Unmounting a view, on
 * the other hand, no longer stops anything: probing belongs to the module and
 * survives every page.
 */
let wait: ReturnType<typeof setTimeout> | null = null
/**
 * Timestamp of the last probe actually launched, which dates the deadline:
 * `lastProbe + periodMs` says when the next one is due. `null` as long as no
 * probe has taken place — arrival on the page, where there is nothing to wait
 * for.
 */
let lastProbe: number | null = null

/**
 * Jiffies counters of the previous probe, to compute a delta — apart from the
 * history, since they only make sense between two consecutive probes and not
 * as a series to display.
 */
const previousJiffies = ref<{ total: number; idle: number } | null>(null)

/**
 * Latch of the console diagnostic: true as soon as a failure has been
 * reported, reset to false on the first success that follows.
 *
 * Probing now runs continuously, from any page and until the tab is closed:
 * an unreachable core without this latch would write a line every 5 s
 * forever — in the order of 17,000 lines a day on a forgotten tab with a
 * flaky link, each one retaining the `Error` object of its request in the
 * console history. Before this store, the warning only ran while the System
 * page was open and visible, which bounded it de facto.
 *
 * So the latch says only one thing: the *transition* to failure, then silence
 * as long as it persists. Re-armed on success, a later outage announces
 * itself again — this is what distinguishes it from a plain "once for the
 * whole life of the page". The "unavailable" line on screen, for its part,
 * stays displayed continuously: it is the one carrying the state, the console
 * only carries the event.
 */
let failureReported = false

/** Last CPU usage computed by `probe`, independently of the history: the CPU
 *  card displays it as soon as it exists, without waiting for the memory to be
 *  readable too (a condition specific to the history). Declared here, next to
 *  `previousJiffies` rather than near its display usage further down:
 *  `probe()` assigns it, and only relies on execution order (the first probe
 *  starts from the store bootstrap in `App.vue`, after the module has been
 *  evaluated) for this to stay safe — a future, more impatient call would hit
 *  the temporal dead zone of a `const` declared afterwards. */
const currentCpuUsage = ref<number | null>(null)

/**
 * Real CPU usage between this probe and the previous one: the counters of
 * `/proc/stat` are cumulative since boot, only a delta between two probes has
 * a meaning (`usage % = 100 × (1 − Δidle / Δtotal)`, clamped to 0-100).
 * `null`: no previous probe yet — the first probe after arriving on the page
 * cannot display a percentage, this is not an outage — or `Δtotal <= 0` (two
 * probes within the same jiffy, or counters that went backwards).
 */
function cpuUsage(s: SystemPayload): number | null {
  const previous = previousJiffies.value
  const total = s.cpu_total_jiffies
  const idle = s.cpu_idle_jiffies
  if (total != null && idle != null) previousJiffies.value = { total, idle }
  if (total == null || idle == null || !previous) return null
  const deltaTotal = total - previous.total
  const deltaIdle = idle - previous.idle
  if (deltaTotal <= 0) return null
  return Math.min(100, Math.max(0, 100 * (1 - deltaIdle / deltaTotal)))
}

/**
 * Sample kept in the history, with the timestamp of the probe (for a future
 * hover, not displayed yet). `null` if either percentage is missing: a machine
 * without readable memory, or whose CPU usage is not computable yet, keeps an
 * empty graph rather than a half-drawn one. A consequence to accept: since the
 * first sample itself requires a delta, the graph only draws its first line on
 * the third probe (two to produce a sample, three to have two of them).
 */
function sample(s: SystemPayload, cpu: number | null): Sample | null {
  if (cpu == null || !s.memory || s.memory.total_kb === 0) return null
  return {
    cpu,
    ram: ((s.memory.total_kb - s.memory.available_kb) / s.memory.total_kb) * 100,
    temp: s.temperature_c ?? null,
    t: Date.now(),
  }
}

/**
 * Probing, where the rest of the SPA receives SSE, and this is deliberate: the
 * `/api/player` stream publishes a state the core produces anyway, whereas
 * these metrics only exist because we ask for them. Pushing them would keep a
 * mostly idle device permanently working, for nobody.
 *
 * Probing starts when the SPA loads and lives until the page is closed:
 * neither going to the background nor unmounting the view stops it, only a
 * power action suspends it. This is a deliberate reversal of the original note
 * ("do not make a mostly idle device work"): a history graph that only
 * measures while being watched teaches nothing, and a read of `/proc` every
 * 5 s costs nothing measurable. The UI, in practice, is rarely open.
 *
 * A failure displays no toast: repeated every 5 seconds, an unreachable core
 * would produce a flood of them. A diagnostic line is enough, like the
 * `audioUnavailable` flag of the settings page.
 */
async function probe() {
  // Entry lock: a probe already in flight (timer ticking faster than the
  // response arrives) does not trigger a second one on top, see the comment
  // on `probeInFlight`.
  if (probeInFlight) return
  // After the lock, not before: a call pushed back by the lock probed
  // nothing, so it must not push the deadline back.
  lastProbe = Date.now()
  const controller = new AbortController()
  probeInFlight = controller
  try {
    const s = await api.get<SystemPayload>('/api/system', { signal: controller.signal })
    state.value = s
    unavailable.value = false
    // Re-arm the latch: the next outage will be entitled to its line.
    failureReported = false
    const cpu = cpuUsage(s)
    currentCpuUsage.value = cpu
    const p = sample(s, cpu)
    if (p) {
      history.value.push(p)
      if (history.value.length > CAPACITY) history.value.shift()
    }
  } catch (e) {
    // A cancellation by `stop()` (period change, suspension for a power
    // action) also rejects the `fetch`: this is not a core failure, just our
    // own request cut short, so no "unavailable" line for that.
    if (controller.signal.aborted) return
    unavailable.value = true
    // One line per outage, not one per probe: see `failureReported`.
    if (!failureReported) {
      failureReported = true
      console.warn('GET /api/system unavailable, staying quiet until it answers again', e)
    }
  } finally {
    if (probeInFlight === controller) probeInFlight = null
  }
}

function start() {
  // `paused`: a power action in progress has already stopped normal probing
  // (see `confirm`); letting it resume here — e.g. on a period change during
  // a shutdown or a restart of the service — would display an alarming
  // network error on a shutdown going exactly as requested, or would probe
  // twice alongside `waitForReturn`.
  //
  // No more `document.hidden` here: probing goes on in the background, that
  // is the reason this store exists. Measured and accepted reservation —
  // browsers throttle the timers of a hidden tab (at least 1 s, and about one
  // tick per minute beyond a few minutes), so samples taken during an absence
  // are spaced out, not regular. Since the x axis is drawn from the
  // timestamps (`xValues`, in `views/sparkline.ts`), the plot stays correct;
  // and so does the CPU delta, the jiffies of `/proc/stat` being cumulative —
  // a one-minute gap gives an average over the minute, not a wrong figure.
  if (paused || timer !== null) return
  if (wait !== null) return
  // Resume at the deadline, not on the spot: changing the period must not
  // cost a probe. We only probe immediately if the new rhythm makes the
  // previous probe already stale — going from 30 s to 1 s two seconds after
  // the last one, for instance. Otherwise we wait for the time that was left
  // to run, then the regular rhythm resumes.
  //
  // The rule also holds for the resumption after a power action, and that is
  // intended: a suspension shorter than the period leaves on screen figures
  // the page itself still deems fresh, whereas a longer interruption does
  // trigger an immediate probe.
  const remaining =
    lastProbe === null ? 0 : Math.max(0, lastProbe + periodMs.value - Date.now())
  if (remaining === 0) {
    void probe()
    timer = setInterval(probe, periodMs.value)
    return
  }
  wait = setTimeout(() => {
    wait = null
    void probe()
    timer = setInterval(probe, periodMs.value)
  }, remaining)
}

function stop() {
  if (timer !== null) {
    clearInterval(timer)
    timer = null
  }
  if (wait !== null) {
    clearTimeout(wait)
    wait = null
  }
  // Cancel a probe still in flight: without this, a period change would let
  // an older response land after the one of the new rhythm and overwrite
  // `state`/`previousJiffies` with stale data.
  if (probeInFlight) {
    probeInFlight.abort()
    probeInFlight = null
  }
}

/**
 * Probing paused by a power action in progress.
 *
 * Replaces the `inProgress !== null` test that `start()` did when everything
 * lived in the view: probing is now shared by the whole SPA and can no longer
 * read the state of one page. The guard stays unique and stays here — it is
 * what prevents a period change from relighting probing on a core that has
 * just been switched off.
 *
 * Counterpart not to lose sight of: left at `true`, it freezes the graph for
 * *all* pages, not only the one that paused, and nothing on screen explains
 * it — `unavailable` stays false, the graph keeps its last points. Every exit
 * path of a power action must therefore call `resume()`. A single exception:
 * the **shutdown** of the machine, where the device leaves for good. The
 * reboot of the machine is not one, against intuition: the Pi comes back in
 * 20 to 40 s and the tab, for its part, has not moved — `confirm` therefore
 * waits for it as it waits for the return of the service.
 */
let paused = false

/**
 * Without `export`, deliberately, like `stop()`: the only door is the object
 * returned by `useMetrics()`. An `import { pause }` would let any module
 * freeze the probing of the whole SPA without going through the view that
 * answers for it — exactly what the privacy of `stop()` exists to prevent,
 * and the risk is not theoretical: a `paused` left at `true` has no other
 * remedy than a full reload of the page.
 */
function pause(): void {
  paused = true
  stop()
}

function resume(): void {
  paused = false
  start()
}

/**
 * View value (string, seconds) for the period selector. The change restarts
 * probing by going back through `start()` — without bypassing it: it is the
 * one that refuses to restart during a power action in progress, and this
 * guard must stay unique.
 */
const period = computed({
  get: () => String(periodMs.value / 1000),
  set: (v: string) => {
    const ms = Number(v) * 1000
    // Choosing again the period already active must not retrigger anything:
    // without this safeguard, every selection — even without change — stopped
    // and relaunched probing, with a superfluous immediate probe and a CPU
    // delta window reset for nothing.
    if (ms === periodMs.value) return
    periodMs.value = ms
    stop()
    start()
  },
})

/**
 * Visible window of the history, in minutes: the real duration covered by
 * `history`, measured by the timestamps of its first and last sample, and not
 * the theoretical capacity (`CAPACITY` × period) which assumes an already
 * full buffer. That assumption is false on arrival on the page (empty buffer)
 * and during the `CAPACITY` probes that follow any period change: going from
 * 30 s to 1 s with a full buffer would otherwise display "4 min" while the
 * graph still draws 120 min of samples 30 s apart, and would stay wrong during
 * the `CAPACITY` following probes. Fallback on the theoretical capacity only
 * as long as there is nothing to measure (fewer than two samples).
 */
const windowMinutes = computed(() => {
  const h = history.value
  if (h.length >= 2) return Math.round((h.at(-1)!.t - h[0]!.t) / 60000)
  return Math.round((CAPACITY * (periodMs.value / 1000)) / 60)
})

/**
 * Complete reset. **For tests only**: the state lives at module level, so
 * without this a test leaves its history, its period and its timer to the
 * next one. To be called in a `beforeEach`.
 */
export function resetMetrics(): void {
  stop()
  paused = false
  failureReported = false
  lastProbe = null
  state.value = null
  unavailable.value = false
  history.value = []
  periodMs.value = 5000
  previousJiffies.value = null
  currentCpuUsage.value = null
}

export function useMetrics() {
  return {
    state,
    unavailable,
    history,
    currentCpuUsage,
    periodMs,
    period,
    windowMinutes,
    start,
    pause,
    resume,
  }
}
