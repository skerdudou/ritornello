import { describe, expect, it } from 'vitest'
import type { SystemPayload } from '../types'
import { CPU_WINDOW_MS, evaluateAlerts, type AlertKind } from './systemAlerts'
import type { Sample } from './useMetrics'

function payload(overrides: Partial<SystemPayload> = {}): SystemPayload {
  return {
    temperature_c: 45,
    cpu_mhz: 1200,
    load: [0.2, 0.2, 0.2],
    cpus: 4,
    memory: { total_kb: 1000, available_kb: 800 },
    disk: { total_kb: 1000, available_kb: 900 },
    under_voltage: false,
    under_voltage_since_boot: false,
    uptime_s: 100,
    service_uptime_s: 100,
    hostname: 'pi',
    ip: null,
    os: null,
    kernel: null,
    version: '0.0.0',
    can_power_off: true,
    can_reboot: true,
    logind_reachable: true,
    cpu_total_jiffies: 0,
    cpu_idle_jiffies: 0,
    ...overrides,
  }
}

/** A history of `count` samples `stepMs` apart, all at `cpu` %, ending at `end`. */
function history(cpu: number, durationMs: number, stepMs = 5000, end = 1_000_000): Sample[] {
  const out: Sample[] = []
  for (let t = end - durationMs; t <= end; t += stepMs) out.push({ cpu, ram: 20, temp: 45, t })
  return out
}

function kinds(previous: AlertKind[], s: SystemPayload, h: Sample[] = []): AlertKind[] {
  return evaluateAlerts(new Set(previous), s, h).map((a) => a.kind)
}

describe('evaluateAlerts', () => {
  it('raises nothing on a quiet machine', () => {
    expect(kinds([], payload(), history(3, CPU_WINDOW_MS))).toEqual([])
  })

  it('raises the temperature at 80 °C and keeps it down to 75 °C', () => {
    expect(kinds([], payload({ temperature_c: 79.9 }))).toEqual([])
    expect(kinds([], payload({ temperature_c: 80 }))).toEqual(['temperature'])
    // Hysteresis: between the two thresholds, the previous state decides —
    // a sensor hovering around 80 must not make the triangle blink.
    expect(kinds(['temperature'], payload({ temperature_c: 76 }))).toEqual(['temperature'])
    expect(kinds([], payload({ temperature_c: 76 }))).toEqual([])
    expect(kinds(['temperature'], payload({ temperature_c: 74.9 }))).toEqual([])
  })

  it('carries the value it judged, rounded for the sentence', () => {
    const [alert] = evaluateAlerts(new Set(), payload({ temperature_c: 82.46 }), [])
    expect(alert).toEqual({ kind: 'temperature', value: 82.5 })
  })

  it('raises memory and disk at 90 % used, keeps them down to 85 %', () => {
    const full = { total_kb: 1000, available_kb: 100 }
    const between = { total_kb: 1000, available_kb: 120 }
    const low = { total_kb: 1000, available_kb: 160 }
    expect(kinds([], payload({ memory: full, disk: full }))).toEqual(['memory', 'disk'])
    expect(kinds(['memory', 'disk'], payload({ memory: between, disk: between }))).toEqual(['memory', 'disk'])
    expect(kinds([], payload({ memory: between, disk: between }))).toEqual([])
    expect(kinds(['memory', 'disk'], payload({ memory: low, disk: low }))).toEqual([])
  })

  it('ignores a usage it cannot compute', () => {
    expect(kinds([], payload({ memory: null, disk: { total_kb: 0, available_kb: 0 }, temperature_c: null }))).toEqual([])
  })

  it('raises the instantaneous under-voltage, not the one since boot', () => {
    expect(kinds([], payload({ under_voltage: true }))).toEqual(['voltage'])
    expect(kinds([], payload({ under_voltage: false, under_voltage_since_boot: true }))).toEqual([])
  })

  // The case this alert was written for: an infrared receiver gone wrong, its
  // process flooding the system log in a loop. One process saturates **one**
  // core — 25 % of a quad-core Pi — so a threshold on the whole machine
  // (90 %) would never have seen it.
  it('raises the CPU when one core is saturated for the whole window', () => {
    expect(kinds([], payload({ cpus: 4 }), history(25, CPU_WINDOW_MS))).toEqual(['cpu'])
    expect(kinds([], payload({ cpus: 4 }), history(20, CPU_WINDOW_MS))).toEqual([])
  })

  it('does not raise the CPU on a burst shorter than the window', () => {
    // Measured on the Pi: an apt-get run reads 26 % of the machine (one core
    // or so). Ninety seconds of it at the end of an otherwise idle window
    // average out at 14 %, under the threshold.
    const idle = history(2, 90_000, 5000, 1_000_000 - 90_000)
    const burst = history(26, 85_000, 5000, 1_000_000)
    expect(kinds([], payload({ cpus: 4 }), [...idle, ...burst])).toEqual([])
    // The same run held for the whole window is no longer a burst.
    expect(kinds([], payload({ cpus: 4 }), history(26, CPU_WINDOW_MS))).toEqual(['cpu'])
  })

  it('does not judge the CPU before the history covers the window', () => {
    // Arrival on the page: two minutes of saturation are not "several minutes".
    expect(kinds([], payload({ cpus: 4 }), history(100, CPU_WINDOW_MS - 10_000))).toEqual([])
  })

  it('weights each sample by the time it covers, not by its count', () => {
    // Thirteen samples 5 s apart at 100 % over the first minute, then one
    // sample covering the two remaining minutes at 0 % (a hidden tab,
    // throttled to one probe now and then): a plain mean says 93 %, the real
    // average is a third.
    const end = 1_000_000
    const busy = history(100, 60_000, 5000, end - (CPU_WINDOW_MS - 60_000))
    const h = [...busy, { cpu: 0, ram: 20, temp: 45, t: end }]
    expect(kinds([], payload({ cpus: 1 }), h)).toEqual([])
  })

  it('keeps the CPU raised down to 70 % of its threshold', () => {
    // Threshold on 4 cores: 22.5 %; release below 15.75 %.
    expect(kinds(['cpu'], payload({ cpus: 4 }), history(17, CPU_WINDOW_MS))).toEqual(['cpu'])
    expect(kinds(['cpu'], payload({ cpus: 4 }), history(15, CPU_WINDOW_MS))).toEqual([])
  })

  it('treats an unknown core count as a single core', () => {
    // No false alarm on a machine that does not say how many cores it has.
    expect(kinds([], payload({ cpus: null }), history(25, CPU_WINDOW_MS))).toEqual([])
    expect(kinds([], payload({ cpus: null }), history(95, CPU_WINDOW_MS))).toEqual(['cpu'])
  })
})
