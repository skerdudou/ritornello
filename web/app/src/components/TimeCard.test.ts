import { flushPromises, mount } from '@vue/test-utils'
import { Select, SelectItem } from '@ritornello/ui'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { SystemPayload } from '../types'
import TimeCard from './TimeCard.vue'

// The browser's zone is the one thing a test machine decides on its own:
// pinned here, so the button's presence depends on the fixture only.
const browser = vi.hoisted(() => ({ zone: 'Europe/Paris' }))
vi.mock('../composables/timezone', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../composables/timezone')>()),
  browserZone: () => browser.zone,
}))

const CATALOG = {
  system_time: 'Time',
  system_time_device: 'Device time',
  system_time_zone: 'Time zone',
  system_time_zone_unknown: 'unknown',
  system_time_apply: 'Apply',
  system_time_use_browser: "Use my browser's zone ({zone})",
  system_time_zone_changed: 'Time zone changed.',
  system_time_no_timedated: 'NO-TIMEDATED',
  system_time_not_allowed: 'NOT-ALLOWED',
}

/** 2026-11-04T14:05:30Z. */
const NOW_S = 1_793_801_130

function system(over: Partial<SystemPayload> = {}): SystemPayload {
  return {
    temperature_c: null, cpu_mhz: null, load: null, cpus: null, memory: null, disk: null,
    under_voltage: null, under_voltage_since_boot: null, uptime_s: null, service_uptime_s: 1,
    hostname: null, ip: null, os: null, kernel: null, version: '0.2.0',
    can_power_off: true, can_reboot: true, logind_reachable: true,
    cpu_total_jiffies: null, cpu_idle_jiffies: null, under_voltage_acknowledged: false,
    timezone: 'Etc/UTC',
    utc_offset_seconds: 0,
    device_unix_s: NOW_S,
    timezone_settable: true,
    timedated_reachable: true,
    ...over,
  }
}

let posts: { url: string; body: unknown }[] = []
let postAnswer: () => Response = () => new Response(null, { status: 204 })

beforeEach(async () => {
  vi.useFakeTimers()
  vi.setSystemTime(NOW_S * 1000)
  browser.zone = 'Europe/Paris'
  posts = []
  postAnswer = () => new Response(null, { status: 204 })
  resetCatalog()
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push({ url, body: JSON.parse(String(init.body)) })
        return postAnswer()
      }
      return url === '/api/i18n'
        ? new Response(JSON.stringify(CATALOG), { status: 200 })
        : new Response('', { status: 404 })
    }),
  )
  await useCatalog().reload()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

function mountCard(state: SystemPayload | null, clock24h = true) {
  return mount(TimeCard, {
    props: { state, clock: { date_format: 'day_month_year', clock_24h: clock24h } },
  })
}

describe('TimeCard', () => {
  it("shows the device's wall clock, not the browser's", async () => {
    // Device on UTC+09:00: 14:05 UTC is 23:05 there, whatever the browser is.
    const w = mountCard(system({ timezone: 'Asia/Tokyo', utc_offset_seconds: 32_400 }))
    await flushPromises()
    expect(w.get('[data-time-device]').text()).toBe('23:05')
    expect(w.get('[data-time-zone]').text()).toContain('Asia/Tokyo')
    expect(w.get('[data-time-zone]').text()).toContain('UTC+09:00')
  })

  it('follows the configured 12 h clock', async () => {
    const w = mountCard(system({ timezone: 'Asia/Tokyo', utc_offset_seconds: 32_400 }), false)
    await flushPromises()
    expect(w.get('[data-time-device]').text()).toBe('11:05 PM')
  })

  it('advances with the browser clock between two polls', async () => {
    const w = mountCard(system())
    await flushPromises()
    expect(w.get('[data-time-device]').text()).toBe('14:05')
    // 30 s later the minute has turned, with no new payload.
    await vi.advanceTimersByTimeAsync(30_000)
    expect(w.get('[data-time-device]').text()).toBe('14:06')
  })

  it('names an unknown zone as such, with its offset', async () => {
    const w = mountCard(system({ timezone: null, utc_offset_seconds: -16_200 }))
    await flushPromises()
    expect(w.get('[data-time-zone]').text()).toContain('unknown')
    expect(w.get('[data-time-zone]').text()).toContain('UTC-04:30')
  })

  it("hides the browser's zone button when both zones are the same", async () => {
    browser.zone = 'UTC'
    const w = mountCard(system({ timezone: 'Etc/UTC' }))
    await flushPromises()
    expect(w.find('[data-time-use-browser]').exists()).toBe(false)
  })

  it("offers the browser's zone when they differ, and posts it", async () => {
    const w = mountCard(system({ timezone: 'Etc/UTC' }))
    await flushPromises()
    const button = w.get('[data-time-use-browser]')
    expect(button.text()).toBe("Use my browser's zone (Europe/Paris)")
    await button.trigger('click')
    await flushPromises()
    expect(posts).toEqual([{ url: '/api/system/timezone', body: { timezone: 'Europe/Paris' } }])
    expect(w.find('[data-time-error]').exists()).toBe(false)
  })

  it("lists the zones once opened, the device's and the browser's included", async () => {
    const w = mountCard(system({ timezone: 'Etc/UTC' }))
    await flushPromises()
    // Closed, the list holds nothing: see `choices` in the card.
    expect(w.findAllComponents(SelectItem)).toHaveLength(0)
    await w.findComponent(Select).vm.$emit('update:open', true)
    await flushPromises()
    const values = w.findAllComponents(SelectItem).map((i) => i.props('value'))
    expect(values).toContain('Etc/UTC')
    expect(values).toContain('Europe/Paris')
    expect(values).toContain('Asia/Tokyo')
  })

  it('applies the zone picked in the list', async () => {
    const w = mountCard(system({ timezone: 'Etc/UTC' }))
    await flushPromises()
    await w.findComponent(Select).vm.$emit('update:modelValue', 'America/New_York')
    await w.get('[data-time-apply]').trigger('click')
    await flushPromises()
    expect(posts).toEqual([{ url: '/api/system/timezone', body: { timezone: 'America/New_York' } }])
  })

  it('shows the error of a refused change', async () => {
    postAnswer = () =>
      new Response(JSON.stringify({ error: 'busctl said no' }), { status: 502 })
    const w = mountCard(system({ timezone: 'Etc/UTC' }))
    await flushPromises()
    await w.get('[data-time-use-browser]').trigger('click')
    await flushPromises()
    expect(w.get('[data-time-error]').text()).toBe('busctl said no')
  })

  it('explains, and disables everything, when timedated did not answer', async () => {
    const w = mountCard(system({ timedated_reachable: false, timezone_settable: false }))
    await flushPromises()
    expect(w.get('[data-time-unavailable]').text()).toBe('NO-TIMEDATED')
    expect(w.get('[data-time-apply]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-time-use-browser]').attributes('disabled')).toBeDefined()
  })

  it('explains, and disables everything, when the install did not allow it', async () => {
    const w = mountCard(system({ timedated_reachable: true, timezone_settable: false }))
    await flushPromises()
    expect(w.get('[data-time-unavailable]').text()).toBe('NOT-ALLOWED')
    expect(w.get('[data-time-apply]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-time-use-browser]').attributes('disabled')).toBeDefined()
  })

  it('says nothing and enables the controls when the zone can be changed', async () => {
    const w = mountCard(system())
    await flushPromises()
    expect(w.find('[data-time-unavailable]').exists()).toBe(false)
    expect(w.get('[data-time-use-browser]').attributes('disabled')).toBeUndefined()
  })
})
