import { Select, SelectItem, Slider, Switch, toast } from '@ritornello/ui'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { EqualizerPayload, EqualizerSettings } from '../types'
import EqualizerCard from './EqualizerCard.vue'

// Same approach as `ConfigView.test.ts`: the real kit, only `toast` replaced
// so a refusal can be observed without showing a notification.
vi.mock('@ritornello/ui', async () => {
  const actual = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...actual, toast: { ...actual.toast, error: vi.fn(), success: vi.fn() } }
})

// The catalog is never loaded here: `t` answers with the key itself.

// jsdom does not provide ResizeObserver; reka-ui's Slider uses it to measure
// its track (same stub as AlarmDialog.test.ts).
beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
})

const BANDS = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
const FLAT = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
const ROCK = [4, 3, 2, 0, -1, -1, 0, 2, 3, 4]
const POP = [-1, 0, 2, 3, 3, 2, 0, -1, -1, -1]
const EVENING = [2, 2, 1, 0, 0, 0, -1, -2, -2, -3]

function settings(over: Partial<EqualizerSettings> = {}): EqualizerSettings {
  return {
    enabled: true,
    auto_preamp: true,
    gains: [...FLAT],
    preset: null,
    user_presets: [{ name: 'Evening', gains: [...EVENING] }],
    ...over,
  }
}

function payload(s: EqualizerSettings = settings()): EqualizerPayload {
  return {
    settings: s,
    bands_hz: BANDS,
    builtins: [
      { id: 'flat', name: 'Flat', gains: [...FLAT] },
      { id: 'pop', name: 'Pop', gains: [...POP] },
      { id: 'rock', name: 'Rock', gains: [...ROCK] },
    ],
  }
}

let wrapper: VueWrapper | null = null
let puts: unknown[] = []
let gets = 0
/** How long the fake core takes to answer a `GET` (through `setTimeout`, so fake timers drive it). */
let getDelayMs = 0

/** Loaded only by the tests that read an interpolated sentence. */
const CATALOG = { eq_delete_confirm: 'Delete the preset “{name}”?' }

afterEach(() => {
  wrapper?.unmount()
  wrapper = null
  document.body.innerHTML = ''
  resetCatalog()
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

beforeEach(() => {
  puts = []
  gets = 0
  getDelayMs = 0
  vi.mocked(toast.error).mockClear()
})

/**
 * A fake core: `GET` answers `served()` (read at each call, so a test can
 * change what a reload sees), `PUT` records its body and answers `put()`.
 */
function serve(
  served: () => EqualizerPayload,
  put: () => Promise<Response> | Response = () => new Response(null, { status: 204 }),
) {
  vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
    if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
    expect(url).toBe('/api/equalizer')
    if (init?.method === 'PUT') {
      puts.push(JSON.parse(String(init.body)))
      return put()
    }
    gets += 1
    const body = JSON.stringify(served())
    if (getDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, getDelayMs))
    return new Response(body, { status: 200 })
  }))
}

async function mountCard(p: EqualizerPayload = payload()) {
  if (!vi.isMockFunction(globalThis.fetch)) serve(() => structuredClone(p))
  wrapper = mount(EqualizerCard, { attachTo: document.body })
  await flushPromises()
  return wrapper
}

const sliders = (w: VueWrapper) => w.findAllComponents(Slider)
const presetSelect = (w: VueWrapper) => w.getComponent(Select)
const presetLabel = (w: VueWrapper) => w.get('[data-eq-preset]').text()
/** The kit's `Switch`, by the `data-*` it carries (0: enabled, 1: auto preamp). */
const switchOf = (w: VueWrapper, attr: string) => {
  const all = w.findAllComponents(Switch)
  const found = all.find((s) => s.find(`[${attr}]`).exists())
  expect(found, `${attr} among ${all.length} switches`).toBeDefined()
  return found!
}

async function moveSlider(w: VueWrapper, band: number, gain: number) {
  await sliders(w)[band]!.vm.$emit('update:modelValue', [gain])
}

async function pick(w: VueWrapper, preset: string) {
  await presetSelect(w).vm.$emit('update:modelValue', preset)
}

const inBody = <T extends HTMLElement = HTMLElement>(selector: string) =>
  document.body.querySelector<T>(selector)

async function click(selector: string) {
  const e = inBody(selector)
  expect(e, selector).not.toBeNull()
  e!.click()
  await flushPromises()
}

describe('EqualizerCard', () => {
  it('renders ten sliders and the presets', async () => {
    const w = await mountCard()
    expect(sliders(w)).toHaveLength(10)
    expect(w.findAll('[data-eq-band-label]').map((e) => e.text()))
      .toEqual(['31', '62', '125', '250', '500', '1k', '2k', '4k', '8k', '16k'])
    // Each slider is named by its frequency (the label sits on the thumb).
    const thumbs = w.findAll('[data-slot="slider-thumb"]')
    expect(thumbs.map((e) => e.attributes('aria-label')))
      .toEqual(BANDS.map((hz) => `${hz} Hz`))
    for (const s of sliders(w)) expect(s.props('orientation')).toBe('vertical')
    // Builtins first, in the order received, then the user's.
    expect(presetSelect(w).findAllComponents(SelectItem).map((i) => i.props('value')))
      .toEqual(['builtin:flat', 'builtin:pop', 'builtin:rock', 'user:Evening'])
    // Signed values, under each slider.
    await moveSlider(w, 0, 3)
    await moveSlider(w, 1, -4)
    expect(w.findAll('[data-eq-band-value]').slice(0, 3).map((e) => e.text()))
      .toEqual(['+3', '-4', '0'])
  })

  it('each band reads top to bottom: slider, frequency, value', async () => {
    const w = await mountCard()
    const column = w.findAll('[data-eq-band-label]')[0]!.element.parentElement!
    const order = [...column.children].map((c) => {
      const e = c as HTMLElement
      if (e.dataset.slot === 'slider') return 'slider'
      if ('eqBandLabel' in e.dataset) return 'frequency'
      if ('eqBandValue' in e.dataset) return 'value'
      return 'other'
    })
    expect(order).toEqual(['slider', 'frequency', 'value'])
  })

  it('picking a builtin copies its gains and puts once after 300 ms', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const w = await mountCard()
    // Three changes in quick succession: one PUT, carrying the last state.
    await pick(w, 'builtin:rock')
    expect(sliders(w).map((s) => (s.props('modelValue') as number[])[0])).toEqual(ROCK)
    vi.advanceTimersByTime(100)
    await moveSlider(w, 5, 7)
    vi.advanceTimersByTime(100)
    await pick(w, 'builtin:pop')
    vi.advanceTimersByTime(299)
    await flushPromises()
    expect(puts).toEqual([])
    vi.advanceTimersByTime(1)
    await flushPromises()
    expect(puts).toEqual([{ ...settings(), gains: POP, preset: 'builtin:pop' }])
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(puts).toHaveLength(1)
  })

  it('a change while a PUT is in flight is sent after it, never alongside', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    let release!: () => void
    serve(() => payload(), () => new Promise<Response>((resolve) => {
      release = () => resolve(new Response(null, { status: 204 }))
    }))
    const w = await mountCard()
    await moveSlider(w, 0, 1)
    vi.advanceTimersByTime(300)
    await flushPromises()
    expect(puts).toHaveLength(1)
    await moveSlider(w, 0, 2)
    vi.advanceTimersByTime(300)
    await flushPromises()
    // The first is still unanswered: the second waits.
    expect(puts).toHaveLength(1)
    release()
    await flushPromises()
    expect(puts).toHaveLength(2)
    expect((puts[1] as EqualizerSettings).gains[0]).toBe(2)
    release()
    await flushPromises()
    expect(puts).toHaveLength(2)
  })

  it('moving a slider after a preset shows Custom', async () => {
    const w = await mountCard(payload(settings({ gains: [...ROCK], preset: 'builtin:rock' })))
    expect(presetLabel(w)).toBe('Rock')
    await moveSlider(w, 2, 5)
    // `preset` is still `builtin:rock` in the settings: Custom is decided by
    // the gains drifting from the selected preset's.
    expect(presetLabel(w)).toBe('eq_custom')
    // Moving it back to the preset's value names the preset again.
    await moveSlider(w, 2, ROCK[2]!)
    expect(presetLabel(w)).toBe('Rock')
    // And a null preset is Custom whatever the gains.
    wrapper!.unmount()
    vi.unstubAllGlobals()
    const flat = await mountCard(payload(settings({ gains: [...FLAT], preset: null })))
    expect(presetLabel(flat)).toBe('eq_custom')
  })

  it('deleting the selected user preset keeps the gains and shows Custom', async () => {
    const w = await mountCard(payload(settings({ gains: [...EVENING], preset: 'user:Evening' })))
    expect(presetLabel(w)).toBe('Evening')
    expect(w.get('[data-eq-delete]').attributes('disabled')).toBeUndefined()
    await w.get('[data-eq-delete]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([])
    await click('[data-eq-delete-confirm]')
    expect(puts).toEqual([
      { ...settings(), gains: EVENING, preset: null, user_presets: [] },
    ])
    expect(presetLabel(w)).toBe('eq_custom')
    expect(sliders(w).map((s) => (s.props('modelValue') as number[])[0])).toEqual(EVENING)
    // Nothing left to delete.
    expect(w.get('[data-eq-delete]').attributes('disabled')).toBeDefined()
  })

  it('the delete confirmation names the preset it was opened on', async () => {
    serve(() => payload(settings({ gains: [...EVENING], preset: 'user:Evening' })))
    await useCatalog().reload()
    const w = await mountCard()
    await w.get('[data-eq-delete]').trigger('click')
    await flushPromises()
    const description = () => inBody('[data-eq-delete-dialog]')!.textContent
    expect(description()).toContain('Delete the preset “Evening”?')
    // Whatever turns the list to "Custom" meanwhile (here a band moved, as
    // the confirmation itself does once it lands), the sentence keeps the
    // name the user confirmed, and that is the preset deleted.
    await moveSlider(w, 0, 9)
    expect(presetLabel(w)).toBe('eq_custom')
    expect(description()).toContain('Delete the preset “Evening”?')
    await click('[data-eq-delete-confirm]')
    expect(puts.at(-1)).toMatchObject({ preset: null, user_presets: [] })
  })

  it('delete is offered only on a selected user preset', async () => {
    const w = await mountCard(payload(settings({ gains: [...ROCK], preset: 'builtin:rock' })))
    expect(w.get('[data-eq-delete]').attributes('disabled')).toBeDefined()
  })

  it('saving under an existing name asks before overwriting', async () => {
    const w = await mountCard(payload(settings({ gains: [...ROCK], preset: 'builtin:rock' })))
    await w.get('[data-eq-save]').trigger('click')
    await flushPromises()
    const input = inBody<HTMLInputElement>('[data-eq-name]')!
    input.value = '  evening '
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    await click('[data-eq-name-submit]')
    // Same name as "Evening", whatever the case: nothing written yet.
    expect(puts).toEqual([])
    expect(inBody('[data-eq-overwrite]')).not.toBeNull()
    await click('[data-eq-overwrite-confirm]')
    expect(puts).toEqual([{
      ...settings(),
      gains: ROCK,
      preset: 'user:evening',
      user_presets: [{ name: 'evening', gains: ROCK }],
    }])
    expect(presetLabel(w)).toBe('evening')
  })

  it('editing the name withdraws the overwrite prompt', async () => {
    const w = await mountCard()
    await w.get('[data-eq-save]').trigger('click')
    await flushPromises()
    const input = inBody<HTMLInputElement>('[data-eq-name]')!
    input.value = 'Evening'
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    await click('[data-eq-name-submit]')
    expect(inBody('[data-eq-overwrite]')).not.toBeNull()
    expect(inBody('[data-eq-overwrite-confirm]')!.textContent!.trim()).toBe('eq_overwrite')
    input.value = 'Evening 2'
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    expect(inBody('[data-eq-overwrite]')).toBeNull()
    expect(inBody('[data-eq-overwrite-confirm]')).toBeNull()
    expect(inBody('[data-eq-name-submit]')).not.toBeNull()
    expect(puts).toEqual([])
  })

  it('saving under a new name appends the preset without asking', async () => {
    const w = await mountCard(payload(settings({ gains: [...ROCK] })))
    await w.get('[data-eq-save]').trigger('click')
    await flushPromises()
    const input = inBody<HTMLInputElement>('[data-eq-name]')!
    input.value = 'Night'
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    await click('[data-eq-name-submit]')
    expect(inBody('[data-eq-overwrite]')).toBeNull()
    expect(puts).toEqual([{
      ...settings(),
      gains: ROCK,
      preset: 'user:Night',
      user_presets: [{ name: 'Evening', gains: EVENING }, { name: 'Night', gains: ROCK }],
    }])
  })

  it('reset to flat zeroes the gains and selects the flat builtin', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const w = await mountCard(payload(settings({ gains: [...ROCK], preset: 'builtin:rock' })))
    await w.get('[data-eq-flat]').trigger('click')
    vi.advanceTimersByTime(300)
    await flushPromises()
    expect(puts).toEqual([{ ...settings(), gains: FLAT, preset: 'builtin:flat' }])
    expect(presetLabel(w)).toBe('Flat')
  })

  it('sliders and preamp switch are disabled when the equalizer is off', async () => {
    const off = await mountCard(payload(settings({ enabled: false })))
    expect(sliders(off).every((s) => s.props('disabled') === true)).toBe(true)
    expect(off.findAll('[data-slot="slider"][data-disabled]')).toHaveLength(10)
    expect(off.get('[data-eq-auto-preamp]').attributes('disabled')).toBeDefined()
    expect(off.get('[data-eq-enabled]').attributes('disabled')).toBeUndefined()
    // Switching it on releases them.
    await switchOf(off, 'data-eq-enabled').vm.$emit('update:modelValue', true)
    expect(sliders(off).every((s) => !s.props('disabled'))).toBe(true)
    expect(off.get('[data-eq-auto-preamp]').attributes('disabled')).toBeUndefined()
    // Both switches are named by a label.
    for (const sel of ['[data-eq-enabled]', '[data-eq-auto-preamp]']) {
      const id = off.get(sel).attributes('id')
      expect(id, sel).toBeTruthy()
      expect(off.find(`label[for="${id}"]`).exists(), sel).toBe(true)
    }
  })

  it('the hint follows auto_preamp', async () => {
    const w = await mountCard()
    expect(w.get('[data-eq-hint]').text()).toBe('eq_hint_auto')
    await switchOf(w, 'data-eq-auto-preamp').vm.$emit('update:modelValue', false)
    expect(w.get('[data-eq-hint]').text()).toBe('eq_hint_manual')
  })

  it('a 422 shows the server message and reloads', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    serve(
      () => payload(settings({ gains: [...ROCK], preset: 'builtin:rock' })),
      () => new Response(JSON.stringify({ error: 'equalizer gain out of range' }), { status: 422 }),
    )
    const w = await mountCard()
    expect(gets).toBe(1)
    await moveSlider(w, 0, 9)
    vi.advanceTimersByTime(300)
    await flushPromises()
    expect(puts).toHaveLength(1)
    expect(toast.error).toHaveBeenCalledWith('equalizer gain out of range')
    expect(gets).toBe(2)
    // What the core holds is shown again, not the refused change.
    expect((sliders(w)[0]!.props('modelValue') as number[])[0]).toBe(ROCK[0])
    expect(presetLabel(w)).toBe('Rock')
  })

  it('a slow reload after a refusal is the last word: the refused state is never sent again', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    serve(
      () => payload(settings({ gains: [...ROCK], preset: 'builtin:rock' })),
      () => new Response(JSON.stringify({ error: 'refused' }), { status: 422 }),
    )
    const w = await mountCard()
    getDelayMs = 1000
    await moveSlider(w, 0, 9)
    vi.advanceTimersByTime(300)
    await flushPromises()
    expect(puts).toHaveLength(1)
    expect(gets).toBe(2)
    // A change while the reload is still out: its timer fires before the
    // GET answers, and must not write the refused state on top of it.
    await moveSlider(w, 1, 5)
    vi.advanceTimersByTime(300)
    await flushPromises()
    expect(puts).toHaveLength(1)
    vi.advanceTimersByTime(700)
    await flushPromises()
    // The reload landed: what the core holds is shown, and nothing follows.
    expect(sliders(w).map((s) => (s.props('modelValue') as number[])[0])).toEqual(ROCK)
    vi.advanceTimersByTime(2000)
    await flushPromises()
    expect(puts).toHaveLength(1)
  })

  it('leaving the page within the coalescing window still writes the change', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const w = await mountCard()
    await moveSlider(w, 3, -6)
    vi.advanceTimersByTime(100)
    w.unmount()
    wrapper = null
    await flushPromises()
    expect(puts).toHaveLength(1)
    expect((puts[0] as EqualizerSettings).gains[3]).toBe(-6)
  })

  it('the sliders scroll inside the card, never the page', async () => {
    const w = await mountCard()
    expect(w.get('[data-eq-bands]').classes()).toContain('overflow-x-auto')
  })
})
