import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { CardTitle, Select } from '@ritornello/ui'
import { useCatalog } from '../composables/useCatalog'
import { resetMetrics, useMetrics } from '../composables/useMetrics'
import { firePointer } from '../testing/pointer'
import SystemView from './SystemView.vue'

// Complete payload, reused and tweaked per case. The CPU jiffies default to
// `null`: that is the "the machine does not expose them" case, not a
// failure — tests that need a computable delta provide them explicitly via
// `nextJiffies`.
function payload(overrides: Record<string, unknown> = {}) {
  return {
    temperature_c: 47.8,
    cpu_mhz: 900,
    load: [0.5, 0.4, 0.3],
    cpus: 4,
    memory: { total_kb: 1_000_000, available_kb: 400_000 },
    disk: { total_kb: 30_000_000, available_kb: 24_000_000 },
    under_voltage: false,
    under_voltage_since_boot: false,
    uptime_s: 90_061,
    service_uptime_s: 3_600,
    hostname: 'ritornello',
    ip: '192.168.1.20',
    os: 'Debian GNU/Linux 12 (bookworm)',
    kernel: '6.6.51+rpt-rpi-v7',
    version: '0.1.0',
    can_power_off: true,
    can_reboot: true,
    logind_reachable: true,
    cpu_total_jiffies: null,
    cpu_idle_jiffies: null,
    ...overrides,
  }
}

/**
 * Jiffies counters growing on every call: Δtotal 1000, Δidle 750, hence 25 %
 * usage computable from the second probe on. Used by the history tests, which
 * need a real delta rather than frozen values.
 */
function nextJiffies() {
  let n = 0
  return () => {
    n += 1
    return { cpu_total_jiffies: n * 1000, cpu_idle_jiffies: n * 750 }
  }
}

/** Minimal catalog: the units, the template of the history window and the
 *  one of the errors button are asserted on display. */
const CATALOGUE = {
  system_unit_mb: 'Mo',
  system_unit_gb: 'Go',
  system_unit_day: 'j',
  system_unit_hour: 'h',
  system_unit_minute: 'min',
  system_history_span: '{minutes} min',
  system_errors_all: 'All errors ({count})',
}

/**
 * `fetch` stub answering by URL: the i18n catalog on one side, `/api/system`
 * on the other, `{}` for POSTs (or a refusal, see `postRefusal`). `body`
 * accepts a function, called on every probe, to vary successive responses.
 *
 * The catalog really is served: without it, `createT` returns the key itself
 * and the units would display as "system_unit_day". The test would then be
 * checking the fallback, not the view.
 */
function stub(
  body: unknown | (() => unknown),
  catalog: Record<string, string> = CATALOGUE,
  log: unknown = { lines: [] },
  postRefusal?: string,
) {
  const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      // `postRefusal` provided: the POST fails with that message, as logind
      // does when the polkit rule is missing. Same convention as `log`
      // below — a parameter which, when set, makes the response `ok: false`.
      if (postRefusal !== undefined) {
        return Promise.resolve({
          ok: false,
          status: 502,
          json: async () => ({ error: postRefusal }),
        } as Response)
      }
      return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
    }
    const u = String(url)
    // `/api/logs` told apart from `/api/system`: the page probes both on every
    // round, and serving the metrics payload to the log would give it a
    // missing `lines` — the test would then fail for a reason that is not
    // its own.
    if (u.includes('/api/logs')) {
      if (log === undefined) {
        return Promise.resolve({ ok: false, status: 503, json: async () => ({}) } as Response)
      }
      return Promise.resolve({ ok: true, json: async () => log } as Response)
    }
    // `/api/settings` told apart too: the page fetches it once at mount, to
    // date the log in the configured format. Without this branch, it fell
    // into the fallback below and **consumed a metrics sample**, which
    // shifted every CPU delta computed afterwards — a failure unrelated to
    // what these tests verify.
    if (u.includes('/api/settings')) {
      return Promise.resolve({
        ok: true,
        json: async () => ({ date_format: 'day_month_year', clock_24h: true }),
      } as Response)
    }
    const j = u.includes('/api/i18n')
      ? catalog
      : typeof body === 'function'
        ? (body as () => unknown)()
        : body
    return Promise.resolve({ ok: true, json: async () => j } as Response)
  })
  vi.stubGlobal('fetch', f)
  return f
}

describe('SystemView', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    // The metrics state lives at module level: without a reset, a test
    // inherits the history, the period and the timer of the previous one.
    resetMetrics()
  })
  afterEach(() => {
    resetMetrics()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    // `unstubAllGlobals` does not undo a `spyOn`: without this, the
    // `document.hidden` forced to `true` by the background test would leak
    // into every following test of the file.
    vi.restoreAllMocks()
    // Dialogs are mounted in a portal: without this cleanup, the DOM of one
    // test would leak into the `document.body.querySelector` of the next.
    document.body.innerHTML = ''
  })

  /**
   * Loads the catalog then mounts the view — that is the application's order,
   * `App.vue` reloading the catalog at mount. `attachTo` is required by the
   * dialog tests and harmless for the others.
   */
  async function mountView() {
    await useCatalog().reload()
    // `App.vue` starts the probing when the SPA mounts, not the view: the
    // test harness plays that role, in the same order as the application.
    useMetrics().start()
    const w = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    return w
  }

  it('displays the metrics of the first probe', async () => {
    stub(payload())
    const w = await mountView()
    expect(w.get('[data-system-temperature]').text()).toContain('47.8')
    expect(w.get('[data-system-frequency]').text()).toContain('900')
    expect(w.get('[data-system-cores]').text()).toBe('4')
    expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
    expect(w.get('[data-system-kernel]').text()).toBe('6.6.51+rpt-rpi-v7')
    // 90 061 s = 1 day 1 hour, at most two units.
    expect(w.get('[data-system-uptime]').text()).toBe('1 j 1 h')
    // 600 000 KiB used out of 1 000 000, rounded to MB, then the ratio in parentheses.
    expect(w.get('[data-system-memory]').text()).toBe('586 / 977 Mo (60 %)')
    w.unmount()
  })

  it('displays a dash for what the machine does not expose', async () => {
    stub(payload({ temperature_c: null, cpu_mhz: null, ip: null }))
    const w = await mountView()
    expect(w.get('[data-system-temperature]').text()).toBe('—')
    expect(w.get('[data-system-frequency]').text()).toBe('—')
    expect(w.get('[data-system-ip]').text()).toBe('—')
    w.unmount()
  })

  it('reports an unreachable core without emptying the page', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('network')))
    const w = await mountView()
    expect(w.find('[data-system-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('arrives with an empty history and fills it probe after probe', async () => {
    const jiffies = nextJiffies()
    stub(() => payload(jiffies()))
    const w = await mountView()
    // A single sample: the chart is **already there** but draws nothing, and
    // its legend announces "—" rather than "0 %". That is what avoids the
    // layout jump at the second probe, when a waiting message used to give
    // way all at once to a 96 px figure.
    expect(w.find('[data-system-history]').exists()).toBe(true)
    expect(w.get('[data-system-history]').html()).not.toContain('M0.00,')
    expect(w.get('[data-system-history-legend]').text()).toContain('—')
    // A sample requires a jiffies delta: the first probe only sets the
    // reference, pushing nothing. Two more probes (10 s) therefore push two,
    // enough to draw a line.
    await vi.advanceTimersByTimeAsync(10000)
    await flushPromises()
    expect(w.find('[data-system-history]').exists()).toBe(true)
    expect(w.get('[data-system-history]').html()).toContain('M0.00,')
    w.unmount()
  })

  it('caps the history at 240 samples', async () => {
    const jiffies = nextJiffies()
    stub(() => payload(jiffies()))
    const w = await mountView()
    // Mounting pushes no sample: a jiffies delta is needed, hence a first
    // reference probe before anything is computable. 241 more probes (5 s
    // period) therefore push 241 samples, the 241st evicting the oldest via
    // `shift()`: exactly 240 must remain, i.e. 239 "L" commands in the path
    // (an "M" then n-1 "L").
    await vi.advanceTimersByTimeAsync(241 * 5000)
    await flushPromises()
    // The path is carried by the first `<path>`, `[data-system-history]`
    // marking the `<svg>` that contains both of them.
    const d = w.get('[data-system-history] path').attributes('d')!
    expect((d.match(/L/g) ?? []).length).toBe(239)
    w.unmount()
  })

  it('spaces the points by real time when the period changes midway', async () => {
    // The requested behaviour: after switching from 5 s to 1 s, the old
    // samples stay widely spaced and the recent ones tighten. Rank-based
    // placement would have made them all equal, passing 5 s of history off
    // as 1 s. This test covers the **wiring** (both paths, the hover line and
    // the popover share `chartXValues`); the computation itself is tested in
    // `sparkline.test.ts`.
    const jiffies = nextJiffies()
    stub(() => payload(jiffies()))
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(4 * 5000)
    await flushPromises()
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    await vi.advanceTimersByTimeAsync(4 * 1000)
    await flushPromises()

    const d = w.get('[data-system-history] path').attributes('d')!
    const xs = [...d.matchAll(/[ML](-?\d+\.\d+),/g)].map((m) => Number(m[1]))
    // Enough samples on both sides of the change for the comparison to be
    // meaningful.
    expect(xs.length).toBeGreaterThanOrEqual(5)
    const gaps = xs.slice(1).map((x, i) => x - (xs[i] ?? 0))
    const first = gaps[0] ?? 0
    const last = gaps.at(-1) ?? 0
    // Theoretical ratio 5 (5 s against 1 s); we require 3 to allow for the
    // real time that also elapses under `shouldAdvanceTime`.
    expect(first).toBeGreaterThan(last * 3)
    w.unmount()
  })

  it('the probing survives unmounting the view', async () => {
    // The probing no longer belongs to the page but to the module store,
    // shared by the whole SPA: a view going away is no reason to stop
    // measuring. Leaving the System page for the configuration and coming
    // back must find a continuous history, not an empty chart.
    const f = stub(payload())
    const w = await mountView()
    const calls = f.mock.calls.length
    w.unmount()
    // Three 5 s periods: the store timer still ticks.
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBeGreaterThan(calls)
  })

  it('keeps probing when the tab goes to the background, and starts in an already hidden tab', async () => {
    const f = stub(payload())
    const w = await mountView()
    const before = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    // The tab goes to the background. The probing must no longer stop: the
    // chart is there to tell what happened while one was looking elsewhere.
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(true)
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const after = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    expect(after).toBeGreaterThanOrEqual(before + 3)
    w.unmount()

    // The above proves that going to the background no longer stops an
    // already installed timer; the case specific to the guard of `start()`
    // is the other one: the SPA booting in a tab that is **already** hidden —
    // restored session, tab opened in the background. `document.hidden`
    // still being `true`, `start()` must install the timer anyway.
    resetMetrics()
    const restarted = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    useMetrics().start()
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const hidden = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    expect(hidden).toBeGreaterThanOrEqual(restarted + 3)
  })

  it('keeps the history when leaving the view and coming back', async () => {
    const jiffies = nextJiffies()
    stub(() => payload(jiffies()))
    const w = await mountView()
    // Three probes: the first sets the jiffies reference, the next two push
    // two samples — enough to draw a line.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const before = (w.get('[data-system-history] path').attributes('d')!.match(/L/g) ?? []).length
    expect(before).toBeGreaterThanOrEqual(1)
    w.unmount()

    // The view is unmounted: the probing goes on regardless, and the
    // remounted view finds an already drawn chart instead of starting from
    // scratch.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const remounted = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    const after = (remounted.get('[data-system-history] path').attributes('d')!.match(/L/g) ?? []).length
    expect(after).toBeGreaterThan(before)
    remounted.unmount()
  })

  it('disables the system buttons when polkit is not configured', async () => {
    stub(payload({ can_power_off: false, can_reboot: false }))
    const w = await mountView()
    expect(w.get('[data-power-poweroff]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-power-reboot]').attributes('disabled')).toBeDefined()
    // Restarting the service depends on no authorization.
    expect(w.get('[data-power-restart]').attributes('disabled')).toBeUndefined()
    expect(w.find('[data-power-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('blames polkit when logind answered, logind when it did not answer', async () => {
    // The two causes of a greyed-out button are not fixed the same way, and
    // the sentence that conflates them sends one looking for a polkit rule
    // that is already in place. The test catalog does not carry these keys:
    // `t` returns the key, which is enough to tell the two sentences apart.
    stub(payload({ can_power_off: false, can_reboot: false, logind_reachable: true }))
    const refused = await mountView()
    expect(refused.get('[data-power-unavailable]').text()).toContain('system_power_unavailable')
    refused.unmount()

    stub(payload({ can_power_off: false, can_reboot: false, logind_reachable: false }))
    // Second visit within the same test: the metrics state lives at module
    // level, so the deadline of the previous probe outlives it and `start()`
    // would wait for the end of the period instead of probing right away. We
    // start over from an SPA boot, as the `beforeEach` does.
    resetMetrics()
    const noLogind = await mountView()
    expect(noLogind.get('[data-power-unavailable]').text()).toContain('system_power_no_logind')
    noLogind.unmount()
  })

  it('disables only one of them when a single authorization is missing', async () => {
    // The unavailability message is an OR over the two flags: it must stay
    // displayed even when only one of the two is missing, without disabling
    // the other button.
    stub(payload({ can_power_off: false, can_reboot: true }))
    const w = await mountView()
    expect(w.find('[data-power-unavailable]').exists()).toBe(true)
    expect(w.get('[data-power-poweroff]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-power-reboot]').attributes('disabled')).toBeUndefined()
    w.unmount()
  })

  it('sends nothing before confirmation', async () => {
    const f = stub(payload())
    const w = await mountView()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    // The dialog is mounted in a portal: it lives in document.body.
    expect(document.body.querySelector('[data-power-confirm]')).not.toBeNull()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    document.body.querySelector<HTMLElement>('[data-power-cancel]')!.click()
    await flushPromises()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    w.unmount()
  })

  it('posts the confirmed action then announces the shutdown and stops probing', async () => {
    const f = stub(payload())
    const w = await mountView()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    const posted = f.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'POST')
    expect(posted).toBeDefined()
    expect(posted?.[0]).toBe('/api/system/power')
    expect(JSON.parse(String((posted?.[1] as RequestInit).body))).toEqual({ action: 'poweroff' })
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    // The core goes away: no more probing, otherwise the page would display
    // a network error while everything happens as requested.
    const calls = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBe(calls)
    w.unmount()
  })

  it('does not restart the probing on a period change during a confirmed shutdown', async () => {
    const f = stub(payload())
    const w = await mountView()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    const calls = f.mock.calls.length
    // The period selector stays displayed during the shutdown: nothing stops
    // the user from touching it while the core goes away. It is now the only
    // path within their reach that goes through `start()` again (its setter
    // does `stop()` then `start()`), and the `paused` guard must refuse to
    // restart, otherwise the page would probe a core already gone and
    // display a network error on a shutdown that proceeds as requested. A
    // one-second period against five seconds of advance: with the guard
    // missing, five probes would land here.
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await vi.advanceTimersByTimeAsync(5000)
    expect(f.mock.calls.length).toBe(calls)
    w.unmount()
  })

  it('takes over again when the service restart completes', async () => {
    // Decreasing uptime: the service really came back, which a mere response
    // would not prove (the first probe may still reach the old process). The
    // first two responses carry an uptime well above `before + elapsed`
    // (3600 + a few seconds at most in this test): without that margin, they
    // would already satisfy the new threshold and the wait would stop at the
    // first probe instead of the third, which is precisely what this test
    // must tell apart.
    const responses = [payload(), payload({ service_uptime_s: 9999 }), payload({ service_uptime_s: 2 })]
    let i = 0
    stub(() => responses[Math.min(i++, responses.length - 1)])
    const w = await mountView()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.get('[data-power-progress]').text()).toBeTruthy()
    await vi.advanceTimersByTimeAsync(6000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    w.unmount()
  })

  it('reaches the 30 s cap even if a probe never answers', async () => {
    // After the POST, every GET stays pending forever: a request that
    // connects but never answers. Without the race against a delay in
    // `waitForReturn`, the wait would stay stuck on it instead of reaching
    // the promised cap.
    let posted = false
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posted = true
        return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      }
      if (String(url).includes('/api/i18n')) {
        return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      }
      if (posted) return new Promise<Response>(() => {})
      return Promise.resolve({ ok: true, json: async () => payload() } as Response)
    })
    vi.stubGlobal('fetch', f)
    const w = await mountView()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    await vi.advanceTimersByTimeAsync(35000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    w.unmount()
  })

  it('unmounting during the wait lets the probing resume', async () => {
    // The old process still answers, so its uptime **grows** with the clock —
    // that is what a process still alive does. The return condition compares
    // it to `before + elapsed`: an uptime that follows the clock can never
    // drop below that threshold, so the wait runs until the view is
    // unmounted. A frozen sample, on the other hand, would end up being taken
    // for a successful restart and this test would no longer say what it
    // claims.
    //
    // The probing belongs to the store, shared by the whole SPA: leaving the
    // page during a service restart cannot freeze the measurement for all
    // the others. `waitForReturn` must therefore hand back to `resume()` on
    // its exit by unmount as on its exit by cap; without that, `paused`
    // stays true for the life of the page and no later `start()` ever
    // restarts.
    const startedAt = Date.now()
    const f = stub(() => payload({ service_uptime_s: 3600 + Math.floor((Date.now() - startedAt) / 1000) }))
    const w = await mountView()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    w.unmount()
    // Enough time for the loop to notice the unmount on the next round and
    // resume the regular probing.
    await vi.advanceTimersByTimeAsync(10000)
    const calls = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(30000)
    expect(f.mock.calls.length).toBeGreaterThan(calls)
  })

  it('a confirmed device reboot resumes the probing when the machine comes back', async () => {
    // The Pi goes away then **comes back** in 20 to 40 s, while the tab has
    // not moved: the machine reboot is therefore awaited like the service
    // restart, only the cap differs. Without this wait, `paused` stayed true
    // for the life of the page — the chart frozen on the samples from before
    // the reboot, on *every* page, `unavailable` still false and therefore
    // nothing on screen to explain it.
    //
    // Same uptime gradation as for the service restart: the first two
    // responses carry an uptime well above `before + elapsed`, only the third
    // proves a service restarted from zero — which `service_uptime_s` also
    // does after a full reboot, since the service restarts with the machine.
    const responses = [payload(), payload({ service_uptime_s: 9999 }), payload({ service_uptime_s: 2 })]
    let i = 0
    const f = stub(() => responses[Math.min(i++, responses.length - 1)])
    const w = await mountView()
    await w.get('[data-power-reboot]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.get('[data-power-progress]').text()).toBeTruthy()
    await vi.advanceTimersByTimeAsync(6000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    // The assertion that counts: the regular probing did resume.
    const calls = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(
      f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length,
    ).toBeGreaterThan(calls)
    w.unmount()
  })

  it('a refused power POST hands back to the probing', async () => {
    // An ordinary path on this hardware, not an edge case: a DietPi install
    // without the polkit rule — or with `systemd-logind` masked — refuses the
    // very first `POST /api/system/power`. It is one of the only two exits
    // from the global suspension, and nothing stops: the probing must resume
    // as if the action had never been requested.
    const f = stub(payload(), CATALOGUE, { lines: [] }, 'logind refused')
    const w = await mountView()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    // The action is not running: its message is gone.
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    // The assertion that counts: the suspension was indeed lifted.
    const calls = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(
      f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length,
    ).toBeGreaterThan(calls)
    w.unmount()
  })

  it('computes an exact CPU usage percentage between two probes', async () => {
    // Δtotal 1000, Δidle 250: 100 × (1 − 250/1000) = 75 %.
    const responses = [
      payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }),
      payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 750 }),
    ]
    let i = 0
    stub(() => responses[Math.min(i++, responses.length - 1)])
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    w.unmount()
  })

  it('displays a dash for the CPU usage at the first probe', async () => {
    // No previous probe: no delta is computable, and that is not a failure.
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await mountView()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    w.unmount()
  })

  it('displays a dash when the total delta is zero or negative', async () => {
    // Same counters at every probe: Δtotal = 0.
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    w.unmount()
  })

  /** Two probes whose delta yields the wanted percentage. */
  function jiffiesFor(percent: number) {
    const idle = 1000 - percent * 10
    const responses = [
      payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 1000 }),
      payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 1000 + idle }),
    ]
    let i = 0
    return () => responses[Math.min(i++, responses.length - 1)]
  }

  it('the CPU usage bar follows the percentage', async () => {
    stub(jiffiesFor(75))
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    const bar = w.get('[data-system-cpu-bar] div')
    expect(bar.attributes('style')).toContain('width: 75%')
    // Below the threshold: normal colour, for both elements.
    expect(bar.classes()).toContain('bg-primary')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('turns the CPU usage red above 90 percent', async () => {
    stub(jiffiesFor(95))
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('95 %')
    expect(w.get('[data-system-cpu-usage]').classes()).toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-destructive')
    w.unmount()
  })

  it('exactly 90 percent is not an alert yet', async () => {
    // The threshold is strict: otherwise a nominal load would show red.
    stub(jiffiesFor(90))
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('90 %')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-primary')
    w.unmount()
  })

  it('a percentage between 90 and 90 point 5 displays 90 percent without alert', async () => {
    // The displayed label is rounded (`Math.round`): the threshold must
    // compare that same rounded value, not the raw one — otherwise
    // 90 < u <= 90.5 would display "90 %" while being red, contradicting the
    // label itself.
    stub(jiffiesFor(90.2))
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('90 %')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-primary')
    w.unmount()
  })

  it('displays the CPU bar at zero as long as the percentage is unknown', async () => {
    // The bar is there from the first render, empty: otherwise it appeared
    // all at once at the second probe, pushing the layout. Nothing claims
    // "0 %" for all that — the reading line displays "—".
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await mountView()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    expect(w.get('[data-system-cpu-bar] div').attributes('style')).toContain('width: 0%')
    w.unmount()
  })

  it('an in-flight probe prevents a second probe from corrupting the next delta', async () => {
    // Every GET stays pending until the test resolves it explicitly, to
    // simulate a probe that has not answered yet when the timer ticks again.
    const deferred: { resolve: (v: unknown) => void }[] = []
    let n = 0
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      if (String(url).includes('/api/i18n')) return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      // The log is fetched once at mount and does not go through the probing
      // lock: counting it here would measure something other than this test.
      // Same for the settings, fetched once to date that log.
      if (String(url).includes('/api/logs')) return Promise.resolve({ ok: true, json: async () => ({ lines: [] }) } as Response)
      if (String(url).includes('/api/settings')) {
        return Promise.resolve({ ok: true, json: async () => ({ date_format: 'day_month_year', clock_24h: true }) } as Response)
      }
      n += 1
      return new Promise((resolve) => deferred.push({ resolve }))
    })
    vi.stubGlobal('fetch', f)
    const w = await mountView()
    // First probe (triggered by `start()` at mount): in flight.
    expect(n).toBe(1)
    // The timer ticks while this first probe still has not answered: without
    // the lock, that would trigger a second `fetch` on top.
    await vi.advanceTimersByTimeAsync(5000)
    expect(n).toBe(1)
    // The first probe finally answers, setting the jiffies reference.
    deferred[0]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }) })
    await flushPromises()
    // The timer ticks again: the lock is released, a second probe does go out.
    await vi.advanceTimersByTimeAsync(5000)
    expect(n).toBe(2)
    deferred[1]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 750 }) })
    await flushPromises()
    // The delta was not corrupted by an overlap: 75 % exactly, as in the
    // test without overlap above.
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    w.unmount()
  })

  it('places a tick on every full minute of the clock covered by the chart', async () => {
    // System time frozen on a full minute: the ticks mark absolute instants,
    // so their number depends on the **phase** of the window relative to the
    // clock. Without this anchor, the test would be green or red depending
    // on the real time of its run.
    vi.setSystemTime(new Date('2026-08-14T12:00:00.000Z'))
    const jiffies = nextJiffies()
    stub(() => payload(jiffies()))
    const w = await mountView()
    // First sample at 12:00:10 (two probes for a delta), last at 12:00:15:
    // no full minute in that window.
    await vi.advanceTimersByTimeAsync(3 * 5000)
    await flushPromises()
    expect(w.findAll('[data-system-history-tick]').length).toBe(0)
    // 28 more probes lead to 12:02:35: 12:01:00 and 12:02:00 fall within the
    // window, 12:00:00 is before its start.
    await vi.advanceTimersByTimeAsync(28 * 5000)
    await flushPromises()
    expect(w.findAll('[data-system-history-tick]').length).toBe(2)
    w.unmount()
  })

  it('changing the period does not probe right away while the deadline is still running', async () => {
    // 5 s by default, we advance 1 s, then switch to 10 s: the last probe is
    // 1 s old, the new deadline is at 10 s, so nothing must go out before the
    // remaining 9 s.
    const f = stub(payload())
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(1000)
    const before = f.mock.calls.length
    await w.findComponent(Select).vm.$emit('update:modelValue', '10')
    await flushPromises()
    expect(f.mock.calls.length).toBe(before)
    await vi.advanceTimersByTimeAsync(8000)
    expect(f.mock.calls.length).toBe(before)
    await vi.advanceTimersByTimeAsync(1500)
    expect(f.mock.calls.length).toBe(before + 1)
    w.unmount()
  })

  it('changing the period probes immediately if it makes the last probe stale', async () => {
    // 5 s by default, we advance 4 s, then switch to 1 s: the last probe is
    // 4 s old for a 1 s period, so it is already stale and the resumption
    // must be immediate — otherwise the page would stay on figures several
    // periods old after asking to speed up.
    const f = stub(payload())
    const w = await mountView()
    await vi.advanceTimersByTimeAsync(4000)
    const before = f.mock.calls.length
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    expect(f.mock.calls.length).toBe(before + 1)
    w.unmount()
  })

  it('a period change during an in-flight probe does not overwrite a fresher state', async () => {
    type Deferred = { signal: AbortSignal | null | undefined; resolve: (v: unknown) => void }
    const deferred: Deferred[] = []
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      if (String(url).includes('/api/i18n')) return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      // Same reason as in the lock test: the log is fetched a single time at
      // mount, outside the probing, and has no business in `deferred`.
      // Neither do the settings, fetched once to date that log.
      if (String(url).includes('/api/logs')) return Promise.resolve({ ok: true, json: async () => ({ lines: [] }) } as Response)
      if (String(url).includes('/api/settings')) {
        return Promise.resolve({ ok: true, json: async () => ({ date_format: 'day_month_year', clock_24h: true }) } as Response)
      }
      return new Promise((resolve, reject) => {
        const signal = init?.signal
        // A real `AbortSignal` rejects its `fetch` on cancellation: the stub
        // reproduces that behaviour rather than leaving the promise in flight
        // forever.
        signal?.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')))
        deferred.push({ signal, resolve })
      })
    })
    vi.stubGlobal('fetch', f)
    const w = await mountView()
    expect(deferred.length).toBe(1)
    // Period change while this first probe is still in flight.
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    // `stop()` must have cancelled the in-flight request...
    expect(deferred[0]!.signal?.aborted).toBe(true)
    // ... without that cancellation counting as a failure: a request
    // abandoned by our own code is not a failure of the core.
    expect(w.find('[data-system-unavailable]').exists()).toBe(false)
    // ... and the resumption is no longer immediate: the last probe having
    // just been launched, the deadline of the new rhythm (1 s) is not
    // reached. It is what will relaunch.
    expect(deferred.length).toBe(1)
    await vi.advanceTimersByTimeAsync(1000)
    expect(deferred.length).toBe(2)
    // The cancelled request eventually "answers" with data that is however
    // older than what the fresher request already set: it must neither
    // overwrite it nor display the unavailability line — a request abandoned
    // by our own code is not a failure of the core.
    deferred[1]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 1000 }) })
    await flushPromises()
    expect(w.find('[data-system-unavailable]').exists()).toBe(false)
    w.unmount()
  })

  it('re-choosing the already active period does not retrigger the probing', async () => {
    const f = stub(payload())
    const w = await mountView()
    await flushPromises()
    const calls = f.mock.calls.length
    // The initial value of the selector is already "5" (default period):
    // re-choosing it must neither probe immediately nor reset the timer.
    await w.findComponent(Select).vm.$emit('update:modelValue', '5')
    await flushPromises()
    expect(f.mock.calls.length).toBe(calls)
    w.unmount()
  })

  it('changes the probing cadence by changing the period', async () => {
    const f = stub(payload())
    const w = await mountView()
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    const calls = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(3000)
    await flushPromises()
    // At one second, three more probes in 3 s; the previous 5 s period would
    // have produced none over the same duration.
    expect(f.mock.calls.length - calls).toBe(3)
    w.unmount()
  })

  it('orders the cards CPU, Memory, History, Storage, Device, Power', async () => {
    stub(payload())
    const w = await mountView()
    const titles = w.findAllComponents(CardTitle).map((c) => c.text())
    expect(titles[0]).toBe('system_cpu')
    expect(titles[1]).toBe('system_memory')
    expect(titles[2]).toContain('system_history')
    expect(titles[3]).toBe('system_storage')
    expect(titles[4]).toBe('system_device')
    expect(titles[5]).toBe('system_power')
    w.unmount()
  })

  it('displays the load average in the history card', async () => {
    stub(payload())
    const w = await mountView()
    expect(w.get('[data-system-load]').text()).toBe('0.50 · 0.40 · 0.30')
    w.unmount()
  })

  it('displays a dash for the voltage when no probe is present', async () => {
    // `null`: no `rpi_volt` sensor, to be told apart from a healthy power
    // supply (`false`) — the old display conflated the two.
    stub(payload({ under_voltage: null }))
    const w = await mountView()
    const voltage = w.get('[data-system-under-voltage]')
    expect(voltage.text()).toBe('—')
    expect(voltage.classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('displays the nominal voltage when the sensor detects nothing', async () => {
    stub(payload({ under_voltage: false }))
    const w = await mountView()
    const voltage = w.get('[data-system-under-voltage]')
    expect(voltage.text()).toBe('system_voltage_ok')
    expect(voltage.classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('displays the past episode when the sensor is healthy but an episode occurred since boot', async () => {
    // `under_voltage: false` (nothing right now) but `under_voltage_since_boot:
    // true` (the firmware's sticky bit): a third state, distinct from an
    // ongoing under-voltage, without the red of the immediate alert.
    stub(payload({ under_voltage: false, under_voltage_since_boot: true }))
    const w = await mountView()
    const voltage = w.get('[data-system-under-voltage]')
    expect(voltage.text()).toBe('system_voltage_since_boot')
    expect(voltage.classes()).not.toContain('text-destructive')
    // The advice sentence stays reserved for the instantaneous alert.
    expect(w.find('[data-system-under-voltage-avis]').exists()).toBe(false)
    w.unmount()
  })

  it('the instantaneous alert wins over the past episode when both are true', async () => {
    stub(payload({ under_voltage: true, under_voltage_since_boot: true }))
    const w = await mountView()
    expect(w.get('[data-system-under-voltage]').text()).toBe('system_voltage_low')
    w.unmount()
  })

  it('displays the alert in red when under-voltage is detected', async () => {
    stub(payload({ under_voltage: true }))
    const w = await mountView()
    const voltage = w.get('[data-system-under-voltage]')
    // The short word in the grid, not the whole sentence: see the next test
    // for the advice sentence, displayed separately.
    expect(voltage.text()).toBe('system_voltage_low')
    expect(voltage.classes()).toContain('text-destructive')
    w.unmount()
  })

  it('displays the advice sentence below the grid only when the alert is active', async () => {
    stub(payload({ under_voltage: false }))
    const w = await mountView()
    expect(w.find('[data-system-under-voltage-avis]').exists()).toBe(false)
    w.unmount()
  })

  it('displays the advice sentence with role status on under-voltage', async () => {
    stub(payload({ under_voltage: true }))
    const w = await mountView()
    const notice = w.get('[data-system-under-voltage-avis]')
    expect(notice.text()).toBe('system_under_voltage')
    expect(notice.attributes('role')).toBe('status')
    w.unmount()
  })

  it('the voltage help button has an accessible name and opens the dialog', async () => {
    stub(payload())
    const w = await mountView()
    const button = w.get('[data-system-voltage-help]')
    expect(button.attributes('aria-label')).toBe('system_voltage_help')
    // Closed at first: the dialog must not impose itself on arrival on the page.
    expect(document.body.querySelector('[role="dialog"]')).toBeNull()
    await button.trigger('click')
    await flushPromises()
    // Mounted in a portal, like the power dialog.
    expect(document.body.textContent).toContain('system_voltage_help_title')
    expect(document.body.textContent).toContain('system_voltage_help_body')
    w.unmount()
  })

  it('the period label corrects itself when the catalog arrives after mount', async () => {
    // Reproduces the real order of a first load: `App.vue` launches the
    // catalog reload at ITS mount, so the view mounts before the response
    // arrives. Every label then corrects itself, `t` being a computed —
    // except the one of the Select trigger, which a `SelectValue` without
    // content froze on the text captured at mount: the list displayed
    // "5 system_unit_second" forever.
    //
    // This test would therefore fail when rendering `<SelectValue />` without
    // content. `mountView()` cannot see it: it loads the catalog BEFORE
    // mounting.
    stub(payload(), {})
    await useCatalog().reload()
    // `App.vue` starts the probing when the SPA mounts, not the view: the
    // test harness plays that role, in the same order as the application.
    useMetrics().start()
    const w = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    expect(w.get('[data-system-period]').text()).toContain('system_unit_second')

    stub(payload(), { ...CATALOGUE, system_unit_second: 's' })
    await useCatalog().reload()
    await flushPromises()
    expect(w.get('[data-system-period]').text()).toBe('5 s')
    w.unmount()
  })

  it('the window label follows the chosen period', async () => {
    stub(payload())
    const w = await mountView()
    expect(w.get('[data-system-history-span]').text()).toBe('20 min')
    await w.findComponent(Select).vm.$emit('update:modelValue', '30')
    await flushPromises()
    expect(w.get('[data-system-history-span]').text()).toBe('120 min')
    w.unmount()
  })

  it('displays the fallback window (capacity × period) as long as the history measures nothing', async () => {
    // Fresh page: no sample pushed yet (only the first reference probe took
    // place), so nothing to measure — fallback on the theoretical capacity at
    // the default period (5 s × 240 = 20 min).
    stub(payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 }))
    const w = await mountView()
    expect(w.get('[data-system-history-span]').text()).toBe('20 min')
    w.unmount()
  })

  it('displays the real duration of the history rather than the capacity once measurable', async () => {
    const jiffies = nextJiffies()
    stub(() => payload(jiffies()))
    const w = await mountView()
    // Three more probes at 5 s: the first only sets the jiffies reference,
    // the next two push two samples 5 real seconds apart — far less than the
    // 20 min the theoretical capacity would promise at this period.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(w.get('[data-system-history-span]').text()).toBe('0 min')
    w.unmount()
  })

  describe('history hover', () => {
    /**
     * Five successive responses with well separated values (cpu 10/30/50/70/90 %,
     * ram 5/25/45/65/85 %): enough to tell the pointed column apart without
     * ambiguity. The very first response only sets the jiffies reference (see
     * `cpuUsage`) and pushes no sample.
     */
    function hoverResponses() {
      const targets: [cpu: number, ram: number][] = [
        [10, 5],
        [30, 25],
        [50, 45],
        [70, 65],
        [90, 85],
      ]
      const responses = [payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 })]
      let total = 0
      let idle = 0
      for (const [cpuTarget, ramTarget] of targets) {
        total += 1000
        idle += 1000 * (1 - cpuTarget / 100)
        responses.push(
          payload({
            cpu_total_jiffies: total,
            cpu_idle_jiffies: idle,
            memory: { total_kb: 1_000_000, available_kb: 1_000_000 * (1 - ramTarget / 100) },
          }),
        )
      }
      return responses
    }

    /**
     * Mounts the view with the five samples above already in the history, and
     * stubs the chart rectangle: under jsdom, `getBoundingClientRect` returns
     * zeros, and every x would collapse to the same index without this stub.
     */
    async function mountWithHistory() {
      const responses = hoverResponses()
      let i = 0
      stub(() => responses[Math.min(i++, responses.length - 1)])
      const w = await mountView()
      await vi.advanceTimersByTimeAsync(5 * 5000)
      await flushPromises()
      const svg = w.get('[data-system-history]')
      vi.spyOn(svg.element, 'getBoundingClientRect').mockReturnValue({
        left: 0, width: 200, top: 0, height: 0, right: 200, bottom: 0, x: 0, y: 0, toJSON: () => {},
      } as DOMRect)
      return { w, svg }
    }

    it('a pointer in the middle of the chart displays the middle sample', async () => {
      const { w, svg } = await mountWithHistory()
      await firePointer(svg, 'pointermove', { clientX: 100 })
      const popover = w.get('[data-system-history-popin]')
      expect(popover.text()).toContain('50 %')
      expect(popover.text()).toContain('45 %')
      w.unmount()
    })

    it('a pointer on the first column displays the first sample', async () => {
      const { w, svg } = await mountWithHistory()
      await firePointer(svg, 'pointermove', { clientX: 0 })
      const popover = w.get('[data-system-history-popin]')
      expect(popover.text()).toContain('10 %')
      expect(popover.text()).toContain('5 %')
      w.unmount()
    })

    it('a pointer on the last column displays the last sample', async () => {
      const { w, svg } = await mountWithHistory()
      await firePointer(svg, 'pointermove', { clientX: 200 })
      const popover = w.get('[data-system-history-popin]')
      expect(popover.text()).toContain('90 %')
      expect(popover.text()).toContain('85 %')
      w.unmount()
    })

    it('the popover appears on hover and disappears on leaving the chart', async () => {
      const { w, svg } = await mountWithHistory()
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      await firePointer(svg, 'pointermove', { clientX: 100 })
      expect(w.find('[data-system-history-popin]').exists()).toBe(true)
      await svg.trigger('pointerleave')
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      w.unmount()
    })

    it('a touch press displays the popover without waiting for a move', async () => {
      // `pointerdown` alone, without `pointermove`: a still tap on a touch
      // screen would never trigger `pointermove`.
      const { w, svg } = await mountWithHistory()
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      await firePointer(svg, 'pointerdown', { clientX: 100 })
      const popover = w.get('[data-system-history-popin]')
      expect(popover.text()).toContain('50 %')
      w.unmount()
    })

    it('an interrupted gesture clears the popover', async () => {
      // `pointercancel`: the gesture is interrupted (a page scroll starting,
      // for instance) without any `pointerup` ever happening.
      const { w, svg } = await mountWithHistory()
      await firePointer(svg, 'pointerdown', { clientX: 100 })
      expect(w.find('[data-system-history-popin]').exists()).toBe(true)
      await svg.trigger('pointercancel')
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      w.unmount()
    })

    it('the hover line follows the pointed column', async () => {
      // The viewBox `WIDTH` is 100, n = 5: the step between columns is 25.
      // Column 2 (hovered by `clientX: 100`, see the middle test above) must
      // therefore place the line at x = 50.
      const { w, svg } = await mountWithHistory()
      await firePointer(svg, 'pointermove', { clientX: 100 })
      const line = w.get('[data-system-history-line]')
      expect(line.attributes('x1')).toBe('50')
      expect(line.attributes('x2')).toBe('50')
      w.unmount()
    })

    it('rounds to the nearest column rather than rounding down', async () => {
      // n = 5 over a width of 200 px: the columns fall at 0, 50, 100, 150,
      // 200. `clientX: 95` (fraction 1.9) and `clientX: 105` (fraction 2.1)
      // must both designate column 2: a `Math.floor` would give 1 for the
      // first and 2 for the second, two different answers where "the
      // nearest" admits only one.
      const { w, svg } = await mountWithHistory()
      await firePointer(svg, 'pointermove', { clientX: 95 })
      let line = w.get('[data-system-history-line]')
      expect(line.attributes('x1')).toBe('50')
      await firePointer(svg, 'pointermove', { clientX: 105 })
      line = w.get('[data-system-history-line]')
      expect(line.attributes('x1')).toBe('50')
      // `clientX: 125` (fraction 2.5, halfway between columns 2 and 3):
      // `Math.round` rounds halves up, hence column 3 (x = 75), which a
      // different "nearest" rounding (to even, for instance) would not
      // necessarily give.
      await firePointer(svg, 'pointermove', { clientX: 125 })
      line = w.get('[data-system-history-line]')
      expect(line.attributes('x1')).toBe('75')
      w.unmount()
    })

    it('the popover is centred by a constant transform, clamped in pixels over the three regimes', async () => {
      // Chart 200 px wide (see the `getBoundingClientRect` stub above),
      // popover 100 px wide (`POPOVER_WIDTH_PX`): the ideal centre cannot go
      // below 50 px nor above 150 px without the popover overflowing the
      // card.
      const { w, svg } = await mountWithHistory()
      // First column (i = 0 of 5): ideal centre at 0 px, clamped to 50 px —
      // the transform stays a constant -50 %, it is the position that is
      // clamped, not a special-cased transform as before this series.
      await firePointer(svg, 'pointermove', { clientX: 0 })
      let popover = w.get('[data-system-history-popin]').element as HTMLElement
      expect(popover.style.transform).toBe('translateX(-50%)')
      expect(popover.style.left).toBe('50px')
      // Middle column (i = 2 of 5): ideal centre at 100 px, in the unclamped
      // band — that was the untested branch before this series, the one where
      // the old code centred without ever clamping.
      await firePointer(svg, 'pointermove', { clientX: 100 })
      popover = w.get('[data-system-history-popin]').element as HTMLElement
      expect(popover.style.transform).toBe('translateX(-50%)')
      expect(popover.style.left).toBe('100px')
      // Last column (i = 4 of 5): ideal centre at 200 px, clamped to 150 px,
      // symmetric to the first column.
      await firePointer(svg, 'pointermove', { clientX: 200 })
      popover = w.get('[data-system-history-popin]').element as HTMLElement
      expect(popover.style.transform).toBe('translateX(-50%)')
      expect(popover.style.left).toBe('150px')
      w.unmount()
    })

    it('hovering a still empty chart displays neither popover nor line', async () => {
      // A single probe: the jiffies reference is set, no sample pushed. The
      // chart is there anyway (it now always is, so the layout does not
      // jump), so it is **hoverable** before having the slightest data —
      // which the old version made impossible by not drawing it. The `< 2`
      // guard of `hoverPointer` and the one of `hoverLineX` therefore become
      // load-bearing: this test pins them.
      stub(payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 }))
      const w = await mountView()
      const svg = w.get('[data-system-history]')
      await firePointer(svg, 'pointermove', { clientX: 100 })
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      expect(w.find('[data-system-history-line]').exists()).toBe(false)
      w.unmount()
    })
  })

  describe('temperature curve', () => {
    it('draws the temperature as a third curve', async () => {
      const jiffies = nextJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await mountView()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      const d = w.get('[data-system-history-temp]').attributes('d')!
      expect(d).not.toBe('')
      // Same scale as the percentages: 47.8 °C reads at mid-height of a
      // 30-unit frame, hence around y = 15.
      expect(d).toMatch(/^M[\d.]+,1[0-9]\.\d\d/)
      w.unmount()
    })

    it('draws nothing without a temperature sensor', async () => {
      const jiffies = nextJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: null }))
      const w = await mountView()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      // The two other curves remain: a machine without a sensor does not lose
      // its chart.
      expect(w.get('[data-system-history] path').attributes('d')).not.toBe('')
      expect(w.get('[data-system-history-temp]').attributes('d')).toBe('')
      w.unmount()
    })

    it('a transient gap in the series hollows the curve without erasing it', async () => {
      // A missing reading no longer erases the whole curve: every present
      // temperature stays on its own abscissa (its timestamp), exactly as in
      // the two other curves, so nothing drifts even when the series has a
      // gap in the middle. `sparklinePath` closes the current sub-path on the
      // `null` and reopens an `M` at the next present point: two SVG
      // sub-paths rather than a missing path.
      const jiffies = nextJiffies()
      let round = 0
      stub(() => payload({ ...jiffies(), temperature_c: round++ === 2 ? null : 47.8 }))
      const w = await mountView()
      await vi.advanceTimersByTimeAsync(20000)
      await flushPromises()
      const d = w.get('[data-system-history-temp]').attributes('d')!
      expect(d).not.toBe('')
      // Two sub-paths: the one before the gap, the one after.
      expect((d.match(/M/g) ?? []).length).toBe(2)
      w.unmount()
    })

    it('announces the temperature in the legend', async () => {
      const jiffies = nextJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await mountView()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      expect(w.get('[data-system-history-legend]').text()).toContain('47.8 °C')
      w.unmount()
    })

    it('announces no temperature in the legend without a sensor', async () => {
      stub(payload({ temperature_c: null }))
      const w = await mountView()
      // No series announced when no curve can exist: the absence of a sensor
      // is known from the first probe, so nothing jumps.
      expect(w.get('[data-system-history-legend]').text()).not.toContain('°C')
      w.unmount()
    })

    it('displays the temperature in the hover popover', async () => {
      const jiffies = nextJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await mountView()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      const svg = w.get('[data-system-history]')
      vi.spyOn(svg.element, 'getBoundingClientRect').mockReturnValue({
        left: 0, width: 200, top: 0, height: 0, right: 200, bottom: 0, x: 0, y: 0, toJSON: () => {},
      } as DOMRect)
      await firePointer(svg, 'pointermove', { clientX: 10 })
      await flushPromises()
      expect(w.get('[data-system-history-popin]').text()).toContain('47.8 °C')
      w.unmount()
    })
  })

  describe('recent errors', () => {
    it('renders one line per log entry, in the order received', async () => {
      // `/api/logs` already returns the most recent first (the core reverses
      // its buffer), the view does not re-sort: it must render the order as
      // is.
      stub(payload(), CATALOGUE, {
        lines: ['WARN most recent', 'WARN oldest'],
      })
      const w = await mountView()
      expect(w.findAll('[data-log-line]').map((l) => l.text())).toEqual([
        'WARN most recent',
        'WARN oldest',
      ])
      w.unmount()
    })

    it('no recent error: no line, and the card stays rendered', async () => {
      stub(payload(), { ...CATALOGUE, recent_errors: 'Recent errors' })
      const w = await mountView()
      expect(w.findAll('[data-log-line]')).toHaveLength(0)
      expect(w.text()).toContain('Recent errors')
      w.unmount()
    })

    it('an unreachable log does not deprive the page of its metrics', async () => {
      // The two fetches are independent, each with its own `.catch`: a
      // failing `/api/logs` must not make the machine look mute — the metrics
      // are precisely what one looks at when the log is missing.
      stub(payload(), CATALOGUE, undefined)
      const w = await mountView()
      expect(w.findAll('[data-log-line]')).toHaveLength(0)
      expect(w.find('[data-system-unavailable]').exists()).toBe(false)
      expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
      w.unmount()
    })

    it('the periodic probing does not fetch the log', async () => {
      // Grafting the log onto `probe()` would lengthen the hold of the
      // "in-flight" lock and change the observed cadence: measured, four
      // cadence tests fell. This test pins the separation.
      const f = stub(payload(), CATALOGUE, { lines: ['WARN an error'] })
      const w = await mountView()
      const atMount = f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length
      expect(atMount).toBe(1)
      await vi.advanceTimersByTimeAsync(20000)
      await flushPromises()
      expect(f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length).toBe(atMount)
      // And the metrics, for their part, did keep being probed.
      expect(
        f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length,
      ).toBeGreaterThan(1)
      w.unmount()
    })
  })

  describe('errors dialog', () => {
    /** Twelve lines: more than the eight of the card, enough for the filter
     *  to have something to discard. */
    const TWELVE = Array.from({ length: 12 }, (_, i) =>
      i === 3 ? 'ERROR mpv socket closed' : `WARN line ${i}`,
    )

    it('the card shows only the eight most recent errors', async () => {
      stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      expect(w.findAll('[data-log-line]')).toHaveLength(8)
      expect(w.findAll('[data-log-line]')[0]!.text()).toBe(TWELVE[0])
      w.unmount()
    })

    it('the button announces the total and is offered from the first error', async () => {
      stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      expect(w.get('[data-logs-all]').text()).toContain('12')
      w.unmount()

      // Three errors: the card already shows them all, and the button is
      // offered anyway. Reported in use — reserved for a long log, the filter
      // was only discovered at the worst moment, when there is too much to
      // read to explore the screen.
      stub(payload(), CATALOGUE, { lines: TWELVE.slice(0, 3) })
      const few = await mountView()
      expect(few.get('[data-logs-all]').text()).toContain('3')
      few.unmount()

      // Empty log: there is nothing to open, the button disappears.
      stub(payload(), CATALOGUE, { lines: [] })
      const empty = await mountView()
      expect(empty.find('[data-logs-all]').exists()).toBe(false)
      empty.unmount()
    })

    it('the dialog lists the whole log', async () => {
      stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // The dialog is rendered in a portal: it lives in document.body.
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('12 / 12')
      w.unmount()
    })

    it('the field filters the list and updates the counter', async () => {
      stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const field = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      field.value = 'mpv'
      field.dispatchEvent(new Event('input'))
      await flushPromises()
      const lines = document.body.querySelectorAll('[data-logs-dialog-line]')
      expect(lines).toHaveLength(1)
      expect(lines[0]!.textContent).toBe('ERROR mpv socket closed')
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('1 / 12')
      w.unmount()
    })

    it('announces the absence of a match', async () => {
      stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const field = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      field.value = 'zzz'
      field.dispatchEvent(new Event('input'))
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(0)
      expect(document.body.querySelector('[data-logs-empty]')).not.toBeNull()
      w.unmount()
    })

    it('fetches the log on opening', async () => {
      const f = stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      const before = f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // One more request, on a user gesture: the log stays outside the
      // periodic probing ("in-flight" lock and CPU delta of `probe`).
      expect(f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length).toBe(before + 1)
      w.unmount()
    })

    it('reopens without the previous filter', async () => {
      stub(payload(), CATALOGUE, { lines: TWELVE })
      const w = await mountView()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const field = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      field.value = 'mpv'
      field.dispatchEvent(new Event('input'))
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(1)

      // Closing through the dialog button — the real gesture, and the only
      // `[data-slot="dialog-close"]` present since only the open dialog is
      // rendered in the portal. Then reopening: the field starts empty again,
      // otherwise the dialog would open on a truncated list without anything
      // on screen to explain it.
      document.body.querySelector<HTMLElement>('[data-slot="dialog-close"]')!.click()
      await flushPromises()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      w.unmount()
    })
  })
})
