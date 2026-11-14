import { Select, SelectItem, Slider } from '@ritornello/ui'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { resetCatalog } from '../composables/useCatalog'
import type { Alarm } from '../types'
import AlarmDialog from './AlarmDialog.vue'
import { newAlarm } from './alarms'

// The catalog is never loaded here: `t` answers with the key itself.

// jsdom does not provide ResizeObserver; reka-ui's Slider uses it to measure
// its track (same stub as PlayerCard.test.ts).
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

function existing(over: Partial<Alarm> = {}): Alarm {
  return {
    id: 4,
    enabled: true,
    hour: 7,
    minute: 30,
    days: ['monday', 'tuesday'],
    target: { kind: 'resume' },
    volume: 25,
    ...over,
  }
}

function mountDialog(alarm: Alarm, isNew = false, over: { clock24h?: boolean; sources?: string[] } = {}) {
  wrapper = mount(AlarmDialog, {
    props: {
      open: true,
      alarm,
      isNew,
      sources: over.sources ?? ['radio', 'cd'],
      clock24h: over.clock24h ?? true,
    },
    attachTo: document.body,
  })
  return wrapper
}

const el = <T extends HTMLElement = HTMLElement>(selector: string) =>
  document.body.querySelector<T>(`[data-alarm-dialog] ${selector}`)

async function click(selector: string) {
  const e = el(selector)
  expect(e, selector).not.toBeNull()
  e!.click()
  await flushPromises()
}

async function type(selector: string, value: string) {
  const input = el<HTMLInputElement>(selector)!
  input.value = value
  input.dispatchEvent(new Event('input'))
  await flushPromises()
}

const saveDisabled = () => el<HTMLButtonElement>('[data-alarm-save]')!.disabled

function saved(w: VueWrapper): Alarm[] {
  return (w.emitted('save') ?? []).map((e) => e[0] as Alarm)
}

describe('AlarmDialog', () => {
  it('refuses to save with no day ticked', async () => {
    const w = mountDialog(newAlarm([]), true)
    await flushPromises()
    expect(saveDisabled()).toBe(false)
    for (const d of ['monday', 'tuesday', 'wednesday', 'thursday', 'friday']) {
      await click(`[data-alarm-day="${d}"]`)
    }
    expect(saveDisabled()).toBe(true)
    expect(el('[data-alarm-no-days]')).not.toBeNull()
    await click('[data-alarm-save]')
    expect(w.emitted('save')).toBeUndefined()
    // One day back is enough.
    await click('[data-alarm-day="sunday"]')
    expect(saveDisabled()).toBe(false)
  })

  it('saves the alarm with every edited field', async () => {
    const w = mountDialog(existing())
    await flushPromises()
    const selects = w.findAllComponents(Select)
    await selects[0]!.vm.$emit('update:modelValue', '6')
    await selects[1]!.vm.$emit('update:modelValue', 'source:cd')
    await type('[data-alarm-minute]', '45')
    await click('[data-alarm-day="monday"]')
    await click('[data-alarm-day="saturday"]')
    await w.findComponent(Slider).vm.$emit('update:modelValue', [55])
    await click('[data-alarm-sleep]')
    await type('[data-alarm-sleep-minutes]', '90')
    await click('[data-alarm-sleep-mode="immediate"]')
    await click('[data-alarm-save]')
    expect(saved(w)).toEqual([
      {
        id: 4,
        enabled: true,
        hour: 6,
        minute: 45,
        days: ['tuesday', 'saturday'],
        target: { kind: 'source', name: 'cd' },
        volume: 55,
        sleep_after: { minutes: 90, mode: 'immediate' },
      },
    ])
  })

  it('keeps a disabled alarm disabled on save', async () => {
    const w = mountDialog(existing({ enabled: false }))
    await flushPromises()
    await click('[data-alarm-save]')
    expect(saved(w)[0]!.enabled).toBe(false)
  })

  it('leaves sleep_after out when "turn off after" is off', async () => {
    const w = mountDialog(existing({ sleep_after: { minutes: 45, mode: 'fade' } }))
    await flushPromises()
    expect(el('[data-alarm-sleep-minutes]')).not.toBeNull()
    await click('[data-alarm-sleep]')
    await click('[data-alarm-save]')
    const out = saved(w)[0]!
    expect('sleep_after' in out).toBe(false)
  })

  it('keeps an untouched sleep_after as it was', async () => {
    const w = mountDialog(existing({ sleep_after: { minutes: 45, mode: 'end_of_track' } }))
    await flushPromises()
    await click('[data-alarm-save]')
    expect(saved(w)[0]!.sleep_after).toEqual({ minutes: 45, mode: 'end_of_track' })
  })

  it('refuses a minute outside 0-59 and a sleep duration outside 1-600', async () => {
    mountDialog(existing())
    await flushPromises()
    await type('[data-alarm-minute]', '60')
    expect(saveDisabled()).toBe(true)
    await type('[data-alarm-minute]', '')
    expect(saveDisabled()).toBe(true)
    await type('[data-alarm-minute]', '59')
    expect(saveDisabled()).toBe(false)
    await click('[data-alarm-sleep]')
    for (const bad of ['0', '601', '1.5']) {
      await type('[data-alarm-sleep-minutes]', bad)
      expect(saveDisabled(), bad).toBe(true)
    }
    for (const good of ['1', '600']) {
      await type('[data-alarm-sleep-minutes]', good)
      expect(saveDisabled(), good).toBe(false)
    }
  })

  it('offers Delete for an existing alarm only, and emits its id', async () => {
    const w = mountDialog(existing())
    await flushPromises()
    await click('[data-alarm-delete]')
    expect(w.emitted('delete')).toEqual([[4]])
    wrapper!.unmount()
    document.body.innerHTML = ''
    mountDialog(newAlarm([]), true)
    await flushPromises()
    expect(el('[data-alarm-delete]')).toBeNull()
  })

  it('offers Resume and each installed source, and keeps a source no longer installed', async () => {
    const w = mountDialog(existing({ target: { kind: 'source', name: 'gone' } }))
    await flushPromises()
    const target = w.findAllComponents(Select)[1]!
    expect(target.props('modelValue')).toBe('source:gone')
    const values = target.findAllComponents(SelectItem).map((i) => i.props('value'))
    expect(values).toEqual(['resume', 'source:radio', 'source:cd', 'source:gone'])
  })

  it('writes the hours the way the device does', async () => {
    const w = mountDialog(existing({ hour: 13 }), false, { clock24h: false })
    await flushPromises()
    expect(el('[data-alarm-hour]')!.textContent).toContain('1 PM')
    await w.setProps({ clock24h: true })
    expect(el('[data-alarm-hour]')!.textContent).toContain('13')
  })

  it('reopens on the saved alarm, not on an abandoned draft', async () => {
    const w = mountDialog(existing())
    await flushPromises()
    await click('[data-alarm-day="saturday"]')
    await w.setProps({ open: false })
    await w.setProps({ open: true })
    await flushPromises()
    expect(el('[data-alarm-day="saturday"]')!.getAttribute('aria-pressed')).toBe('false')
  })
})
