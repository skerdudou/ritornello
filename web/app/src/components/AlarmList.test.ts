import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import { resetCatalog } from '../composables/useCatalog'
import type { Alarm } from '../types'
import AlarmList from './AlarmList.vue'

// The catalog is never loaded here: `t` answers with the key itself.

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
})

let wrapper: VueWrapper | null = null

afterEach(() => {
  wrapper?.unmount()
  wrapper = null
  document.body.innerHTML = ''
  resetCatalog()
})

const ALARMS: Alarm[] = [
  {
    id: 3, enabled: true, hour: 7, minute: 5, days: ['saturday', 'monday'],
    target: { kind: 'resume' }, volume: 20,
  },
  {
    id: 7, enabled: false, hour: 6, minute: 30, days: ['sunday'],
    target: { kind: 'source', name: 'radio' }, volume: 40,
    sleep_after: { minutes: 30, mode: 'fade' },
  },
]

function mountList(
  persist: (a: Alarm[]) => Promise<boolean> = async () => true,
  over: { alarms?: Alarm[]; clock24h?: boolean } = {},
) {
  wrapper = mount(AlarmList, {
    props: {
      alarms: over.alarms ?? structuredClone(ALARMS),
      sources: ['radio', 'cd'],
      clock24h: over.clock24h ?? true,
      persist,
    },
    attachTo: document.body,
  })
  return wrapper
}

const inDialog = <T extends HTMLElement = HTMLElement>(selector: string) =>
  document.body.querySelector<T>(`[data-alarm-dialog] ${selector}`)

async function clickInDialog(selector: string) {
  const e = inDialog(selector)
  expect(e, selector).not.toBeNull()
  e!.click()
  await flushPromises()
}

describe('AlarmList', () => {
  it('lists the alarms in time order, with week-ordered day chips and the target', () => {
    const w = mountList()
    expect(w.findAll('[data-alarm-time]').map((e) => e.text())).toEqual(['06:30', '07:05'])
    const first = w.get('[data-alarm-line="3"]')
    expect(first.findAll('[data-alarm-chip]').map((c) => c.attributes('data-alarm-chip')))
      .toEqual(['monday', 'saturday'])
    expect(first.get('[data-alarm-target-label]').text()).toBe('alarm_target_resume')
    expect(w.get('[data-alarm-line="7"] [data-alarm-target-label]').text()).toBe('radio')
  })

  it('writes the time the way the device does', () => {
    const w = mountList(undefined, { clock24h: false })
    expect(w.findAll('[data-alarm-time]').map((e) => e.text())).toEqual(['6:30 AM', '7:05 AM'])
  })

  it('says so when there is no alarm', () => {
    const w = mountList(undefined, { alarms: [] })
    expect(w.find('[data-alarms-empty]').exists()).toBe(true)
  })

  it('the switch on a line writes the list with only that alarm flipped', async () => {
    // Both on before the click: a write that switched every alarm off would
    // otherwise pass for one that switched only this one.
    const both = structuredClone(ALARMS)
    both[1]!.enabled = true
    const persist = vi.fn(async () => true)
    const w = mountList(persist, { alarms: structuredClone(both) })
    await w.get('[data-alarm-enabled="7"]').trigger('click')
    await flushPromises()
    const expected = structuredClone(both)
    expected[1]!.enabled = false
    expect(persist).toHaveBeenCalledTimes(1)
    expect(persist).toHaveBeenCalledWith(expected)
  })

  it('a new alarm takes the highest id plus one, and is appended', async () => {
    const persist = vi.fn(async (_: Alarm[]) => true)
    const w = mountList(persist)
    await w.get('[data-alarm-add]').trigger('click')
    await flushPromises()
    await clickInDialog('[data-alarm-save]')
    const next = persist.mock.calls[0]![0]
    expect(next).toHaveLength(3)
    expect(next.slice(0, 2)).toEqual(ALARMS)
    expect(next[2]!.id).toBe(8)
    expect(next[2]!.enabled).toBe(true)
  })

  it('the first alarm takes id 1', async () => {
    const persist = vi.fn(async (_: Alarm[]) => true)
    const w = mountList(persist, { alarms: [] })
    await w.get('[data-alarm-add]').trigger('click')
    await flushPromises()
    await clickInDialog('[data-alarm-save]')
    expect(persist.mock.calls[0]![0].map((a) => a.id)).toEqual([1])
  })

  it('an edit replaces that alarm in place and closes the dialog', async () => {
    const persist = vi.fn(async (_: Alarm[]) => true)
    const w = mountList(persist)
    await w.get('[data-alarm-open="3"]').trigger('click')
    await flushPromises()
    await clickInDialog('[data-alarm-day="friday"]')
    await clickInDialog('[data-alarm-save]')
    const next = persist.mock.calls[0]![0]
    expect(next.map((a) => a.id)).toEqual([3, 7])
    expect(next[0]!.days).toEqual(['monday', 'friday', 'saturday'])
    expect(next[1]).toEqual(ALARMS[1])
    expect(inDialog('[data-alarm-save]')).toBeNull()
  })

  it('delete removes that alarm only', async () => {
    const persist = vi.fn(async (_: Alarm[]) => true)
    const w = mountList(persist)
    await w.get('[data-alarm-open="7"]').trigger('click')
    await flushPromises()
    await clickInDialog('[data-alarm-delete]')
    expect(persist.mock.calls[0]![0]).toEqual([ALARMS[0]])
    expect(inDialog('[data-alarm-delete]')).toBeNull()
  })

  it('a refused write keeps the dialog open on the draft', async () => {
    const persist = vi.fn(async (_: Alarm[]) => false)
    const w = mountList(persist)
    await w.get('[data-alarm-open="3"]').trigger('click')
    await flushPromises()
    await clickInDialog('[data-alarm-day="friday"]')
    await clickInDialog('[data-alarm-save]')
    expect(persist).toHaveBeenCalledTimes(1)
    expect(inDialog('[data-alarm-day="friday"]')!.getAttribute('aria-pressed')).toBe('true')
  })
})
