import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import SleepTimerKey from './SleepTimerKey.vue'
import { formatRemaining, remainingSeconds } from './sleepTimer'
import { resetCatalog } from '../composables/useCatalog'
import type { PlayerPayload, SleepMode, SleepTimerState } from '../types'

// The catalog is never loaded here: `t` answers with the key itself, which is
// what the assertions on texts read.

/** A whole second, so the countdown's rounding is not what the test measures. */
const NOW_S = 1_800_000_000

function payload(over: Partial<PlayerPayload> = {}): PlayerPayload {
  return {
    source: 'radio',
    volume: 60,
    muted: false,
    standby: false,
    preset: null,
    preset_count: null,
    preset_name: null,
    status: null,
    overlay: null,
    artist: null,
    title: null,
    album: null,
    duration_s: null,
    origin: null,
    cover_href: null,
    cover_origin: null,
    position_s: null,
    seekable: false,
    can_eject: false,
    has_finite_list: false,
    random: false,
    ...over,
  }
}

function timer(over: Partial<SleepTimerState> = {}): SleepTimerState {
  return { ends_at_unix_s: NOW_S + 1800, mode: 'fade', phase: 'armed', ...over }
}

let wrapper: VueWrapper | null = null

afterEach(() => {
  wrapper?.unmount()
  wrapper = null
  document.body.innerHTML = ''
  resetCatalog()
  vi.useRealTimers()
})

function mountKey(state: PlayerPayload | null, mode: SleepMode = 'fade') {
  wrapper = mount(SleepTimerKey, { props: { state, mode }, attachTo: document.body })
  return wrapper
}

const key = (w: VueWrapper) => w.get('[data-sleep-timer-key]')
const menu = () => document.body.querySelector('[data-sleep-timer-menu]')
const inMenu = (selector: string) => document.body.querySelector<HTMLElement>(`[data-sleep-timer-menu] ${selector}`)

async function openMenu(w: VueWrapper) {
  await key(w).trigger('click')
  await flushPromises()
  expect(menu()).not.toBeNull()
}

async function click(selector: string) {
  const el = inMenu(selector)
  expect(el, selector).not.toBeNull()
  el!.click()
  await flushPromises()
}

describe('SleepTimerKey', () => {
  it('is disabled in standby, where the core ignores SetSleepTimer', () => {
    expect(key(mountKey(payload({ standby: true }))).attributes('disabled')).toBeDefined()
  })

  it('is usable outside standby, and before the first frame', () => {
    // A state not yet received greys nothing, like every other key.
    expect(key(mountKey(payload())).attributes('disabled')).toBeUndefined()
    wrapper!.unmount()
    expect(key(mountKey(null)).attributes('disabled')).toBeUndefined()
  })

  it('starts a timer with the remembered mode and the chosen duration', async () => {
    const w = mountKey(payload(), 'immediate')
    await openMenu(w)
    await click('[data-sleep-minutes="45"]')
    expect(w.emitted('command')).toEqual([[{ cmd: 'SetSleepTimer', arg: { minutes: 45, mode: 'immediate' } }]])
    // The menu closes on its own: the choice is made.
    expect(menu()).toBeNull()
  })

  it('starts a timer with the mode picked in the menu', async () => {
    const w = mountKey(payload(), 'fade')
    await openMenu(w)
    expect(inMenu('[data-sleep-mode="fade"]')!.getAttribute('aria-checked')).toBe('true')
    await click('[data-sleep-mode="end_of_track"]')
    expect(inMenu('[data-sleep-mode="end_of_track"]')!.getAttribute('aria-checked')).toBe('true')
    expect(inMenu('[data-sleep-mode="fade"]')!.getAttribute('aria-checked')).toBe('false')
    await click('[data-sleep-minutes="30"]')
    expect(w.emitted('command')).toEqual([[{ cmd: 'SetSleepTimer', arg: { minutes: 30, mode: 'end_of_track' } }]])
  })

  it('opens on the remembered mode once the settings arrive after mount', async () => {
    // HomeView mounts with a default and reads `/api/settings` afterwards.
    const w = mountKey(payload(), 'fade')
    await w.setProps({ mode: 'end_of_track' })
    await openMenu(w)
    await click('[data-sleep-minutes="15"]')
    expect(w.emitted('command')).toEqual([[{ cmd: 'SetSleepTimer', arg: { minutes: 15, mode: 'end_of_track' } }]])
  })

  it('offers the five durations', async () => {
    const w = mountKey(payload())
    await openMenu(w)
    const minutes = [...document.body.querySelectorAll('[data-sleep-timer-menu] [data-sleep-minutes]')]
      .map((el) => el.getAttribute('data-sleep-minutes'))
    expect(minutes).toEqual(['15', '30', '45', '60', '90'])
  })

  it('counts down by itself: 29:59 one second after a 30-minute deadline', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(NOW_S * 1000)
    const w = mountKey(payload({ sleep_timer: timer() }))
    expect(key(w).text()).toContain('30:00')
    await vi.advanceTimersByTimeAsync(1000)
    expect(key(w).text()).toContain('29:59')
    expect(key(w).attributes('aria-label')).toContain('29:59')
  })

  it('shows only the icon while no timer runs', () => {
    const w = mountKey(payload())
    expect(key(w).text()).toBe('')
    expect(key(w).attributes('aria-label')).toBe('sleep_timer')
  })

  it('offers +15 min and Cancel only while a timer runs', async () => {
    const idle = mountKey(payload())
    await openMenu(idle)
    expect(inMenu('[data-sleep-extend]')).toBeNull()
    expect(inMenu('[data-sleep-cancel]')).toBeNull()
    idle.unmount()
    document.body.innerHTML = ''

    const running = mountKey(payload({ sleep_timer: timer() }))
    await openMenu(running)
    await click('[data-sleep-extend]')
    expect(running.emitted('command')).toEqual([[{ cmd: 'ExtendSleepTimer', arg: 15 }]])

    await openMenu(running)
    await click('[data-sleep-cancel]')
    expect(running.emitted('command')![1]).toEqual([{ cmd: 'CancelSleepTimer' }])
  })

  it('reads the fade label while fading, not a countdown', () => {
    vi.useFakeTimers()
    vi.setSystemTime(NOW_S * 1000)
    const w = mountKey(payload({ sleep_timer: timer({ ends_at_unix_s: NOW_S + 8, phase: 'fading' }) }))
    expect(key(w).text()).toContain('sleep_timer_fading')
    expect(key(w).text()).not.toContain('00:08')
  })

  it('reads the end-of-track label while waiting for the track to end', () => {
    vi.useFakeTimers()
    vi.setSystemTime(NOW_S * 1000)
    // `ends_at` is then the 5-minute cap, not a deadline worth counting down.
    const w = mountKey(payload({ sleep_timer: timer({ ends_at_unix_s: NOW_S + 300, phase: 'waiting_end_of_track' }) }))
    expect(key(w).text()).toContain('sleep_timer_waiting')
    expect(key(w).text()).not.toContain('05:00')
  })
})

describe('sleep timer arithmetic', () => {
  it('never goes below zero, and rounds up', () => {
    expect(remainingSeconds(NOW_S, NOW_S * 1000 + 5000)).toBe(0)
    expect(remainingSeconds(NOW_S + 10, NOW_S * 1000 + 500)).toBe(10)
  })

  it('writes mm:ss under an hour and h:mm:ss from an hour', () => {
    expect(formatRemaining(1799)).toBe('29:59')
    expect(formatRemaining(59)).toBe('00:59')
    expect(formatRemaining(3599)).toBe('59:59')
    expect(formatRemaining(3600)).toBe('1:00:00')
    expect(formatRemaining(5400 + 61)).toBe('1:31:01')
  })
})
