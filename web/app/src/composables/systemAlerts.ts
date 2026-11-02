import type { SystemPayload, SystemUsage } from '../types'
import type { Sample } from './useMetrics'

/**
 * "Something on the machine needs a look" — the warning triangle beside the
 * System entry of both navigations, and the list at the top of that page.
 *
 * Judged in the browser, from the metrics `useMetrics` already probes
 * continuously, and not by the core: the core serves raw facts and keeps no
 * history, whereas the one alert that matters most here — a process stuck in
 * a loop — only exists as a duration.
 *
 * Every alert has two thresholds: it rises at one and falls back at a lower
 * one. A value hovering around a single threshold would make the triangle
 * blink at every probe.
 */
export type AlertKind = 'temperature' | 'cpu' | 'memory' | 'disk' | 'voltage'

/** One raised alert and the value that raised it (°C or %, one decimal), for
 *  the sentence. `null` for under-voltage, which is a yes or a no. */
export interface Alert { kind: AlertKind; value: number | null }

/** A Pi's firmware starts slowing the processor down between 80 and 85 °C. */
const TEMPERATURE_ON = 80
const TEMPERATURE_OFF = 75

/** Share of memory or disk in use. */
const USAGE_ON = 90
const USAGE_OFF = 85

/**
 * How long the processor must stay busy before it is called stuck: three
 * minutes, against a probe every 1 to 30 s. Measured on the Pi, an `apt-get`
 * run holds about one core for a minute or two, and is not a fault.
 *
 * Exported for the tests, which build histories of exactly this length.
 */
export const CPU_WINDOW_MS = 3 * 60_000

/**
 * The CPU alert rises when the average over the window reaches 90 % of
 * **one core** — `90 / cpus` % of the whole machine, 22.5 % on a quad-core
 * Pi — and not 90 % of the machine.
 *
 * This is the case it was written for: an infrared receiver gone wrong, its
 * process writing errors to the system log in a loop. A single runaway
 * process saturates a single core, a quarter of a Pi 3 or 4; a threshold on
 * the whole machine never sees it. A machine that does not say how many cores
 * it has is treated as having one: a missed alert rather than a false one.
 */
const CPU_CORE_SHARE_ON = 90
/** Falls back below 70 % of the rising threshold. */
const CPU_OFF_RATIO = 0.7

function usedPercent(u: SystemUsage | null): number | null {
  if (!u || u.total_kb <= 0) return null
  return ((u.total_kb - u.available_kb) / u.total_kb) * 100
}

/**
 * Average CPU usage over the last `CPU_WINDOW_MS`, or `null` while the
 * history does not reach that far back (arrival on the page).
 *
 * Weighted by time, not by sample count: each sample's `cpu` is the usage
 * since the probe before it (a delta of cumulative jiffies, see
 * `useMetrics`), and in a hidden tab the browser throttles the timer, so one
 * sample may cover a minute where the others cover 5 s.
 */
function cpuAverage(history: Sample[]): number | null {
  const last = history.at(-1)
  if (!last || history.length < 2) return null
  const start = last.t - CPU_WINDOW_MS
  if (history[0]!.t > start) return null
  let weighted = 0
  let covered = 0
  for (let i = 1; i < history.length; i++) {
    const s = history[i]!
    if (s.t <= start) continue
    const span = s.t - Math.max(history[i - 1]!.t, start)
    weighted += s.cpu * span
    covered += span
  }
  return covered > 0 ? weighted / covered : null
}

/** Rises at `on`, falls back below `off`, keeps its previous state between. */
function judge(value: number | null, was: boolean, on: number, off: number): boolean {
  if (value == null) return false
  return was ? value >= off : value >= on
}

const round = (v: number) => Math.round(v * 10) / 10

/**
 * The alerts raised by the latest probe, in a fixed order, given those raised
 * by the previous one (`previous`, for the hysteresis).
 */
export function evaluateAlerts(previous: ReadonlySet<AlertKind>, s: SystemPayload, history: Sample[]): Alert[] {
  const out: Alert[] = []
  const check = (kind: AlertKind, value: number | null, on: number, off: number) => {
    if (judge(value, previous.has(kind), on, off)) out.push({ kind, value: round(value!) })
  }
  check('temperature', s.temperature_c, TEMPERATURE_ON, TEMPERATURE_OFF)
  const cpuOn = CPU_CORE_SHARE_ON / Math.max(1, s.cpus ?? 1)
  check('cpu', cpuAverage(history), cpuOn, cpuOn * CPU_OFF_RATIO)
  check('memory', usedPercent(s.memory), USAGE_ON, USAGE_OFF)
  check('disk', usedPercent(s.disk), USAGE_ON, USAGE_OFF)
  // The instantaneous alarm only: the since-boot bit tells of a past episode,
  // and stays true until the next boot — a triangle nobody could clear.
  if (s.under_voltage === true) out.push({ kind: 'voltage', value: null })
  return out
}
