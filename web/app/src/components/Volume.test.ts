import { Slider } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import Volume from './Volume.vue'

const mounted = (props: Record<string, unknown> = {}) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Volume, { props: { volume: 60, muted: false, disabled: false, ...props } })
}

const press = async (w: ReturnType<typeof mounted>, key: string, opts: { shiftKey?: boolean } = {}) => {
  const handle = w.get('[role="slider"]')
  ;(handle.element as HTMLElement).focus()
  await handle.trigger('keydown', { key, ...opts })
}

describe('Volume', () => {
  beforeAll(() => {
    Element.prototype.setPointerCapture ??= () => {}
    Element.prototype.releasePointerCapture ??= () => {}
    Element.prototype.hasPointerCapture ??= () => true
    // jsdom does not provide ResizeObserver; reka-ui uses it to measure the
    // slider track at mount time (see ProgressBar.test.ts).
    globalThis.ResizeObserver ??= class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  })

  it('shows the value and a handle at that value', async () => {
    const w = mounted()
    // reka-ui resolves aria-valuenow one tick after mounting under jsdom.
    await flushPromises()
    expect(w.get('[data-volume]').text()).toBe('60 %')
    expect(w.get('[role="slider"]').attributes('aria-valuenow')).toBe('60')
  })

  it('shows nothing before the first frame', () => {
    const w = mounted({ volume: null })
    expect(w.get('[data-volume]').text()).toBe('')
  })

  it('the slider moves by steps of 5', () => {
    const w = mounted()
    expect(w.getComponent(Slider).props('step')).toBe(5)
  })

  it('arrows move by 5 and send at once', async () => {
    const w = mounted()
    await press(w, 'ArrowRight')
    expect(w.emitted('set')).toEqual([[65]])
    expect(w.get('[data-volume]').text()).toBe('65 %')
    await press(w, 'ArrowLeft')
    expect(w.emitted('set')).toEqual([[65], [60]])
    await press(w, 'ArrowUp')
    await press(w, 'ArrowDown')
    expect(w.emitted('set')).toEqual([[65], [60], [65], [60]])
  })

  it('two quick presses add up before any frame confirms', async () => {
    const w = mounted()
    await press(w, 'ArrowRight')
    await press(w, 'ArrowRight')
    expect(w.emitted('set')).toEqual([[65], [70]])
  })

  it('a volume off the grid shows as is and the arrows land on the grid', async () => {
    const w = mounted({ volume: 33 })
    await flushPromises()
    expect(w.get('[data-volume]').text()).toBe('33 %')
    expect(w.get('[role="slider"]').attributes('aria-valuenow')).toBe('33')
    await press(w, 'ArrowRight')
    expect(w.emitted('set')).toEqual([[35]])
    const w2 = mounted({ volume: 33 })
    await press(w2, 'ArrowLeft')
    expect(w2.emitted('set')).toEqual([[30]])
  })

  it("Page keys and Shift+arrows move by 10, never by reka's 50", async () => {
    const w = mounted()
    await press(w, 'PageUp')
    expect(w.emitted('set')).toEqual([[70]])
    await press(w, 'PageDown')
    expect(w.emitted('set')).toEqual([[70], [60]])
    const w2 = mounted()
    await press(w2, 'ArrowRight', { shiftKey: true })
    expect(w2.emitted('set')).toEqual([[70]])
    const w3 = mounted({ volume: 33 })
    await press(w3, 'PageUp')
    expect(w3.emitted('set')).toEqual([[40]])
    const w4 = mounted({ volume: 33 })
    await press(w4, 'PageDown')
    expect(w4.emitted('set')).toEqual([[20]])
  })

  it('Home and End go to 0 and 100', async () => {
    const w = mounted()
    await press(w, 'Home')
    expect(w.emitted('set')).toEqual([[0]])
    const w2 = mounted()
    await press(w2, 'End')
    expect(w2.emitted('set')).toEqual([[100]])
  })

  it('the ends clamp and a key that changes nothing sends nothing', async () => {
    const w = mounted({ volume: 98 })
    await press(w, 'ArrowRight')
    expect(w.emitted('set')).toEqual([[100]])
    const w2 = mounted({ volume: 100 })
    await press(w2, 'ArrowRight')
    expect(w2.emitted('set')).toBeUndefined()
    const w3 = mounted({ volume: 2 })
    await press(w3, 'ArrowLeft')
    expect(w3.emitted('set')).toEqual([[0]])
  })

  it('keys do nothing while disabled or before the first frame', async () => {
    const w = mounted({ disabled: true })
    await press(w, 'ArrowRight')
    expect(w.emitted('set')).toBeUndefined()
    const w2 = mounted({ volume: null })
    await press(w2, 'ArrowRight')
    expect(w2.emitted('set')).toBeUndefined()
  })

  it('after a key press, a frame from elsewhere is shown', async () => {
    // Regression pin: reka's post-commit `update:modelValue` used to leave
    // `local` latched, freezing the display on the pressed value.
    const w = mounted()
    await press(w, 'ArrowRight')
    await w.setProps({ volume: 65 })
    await w.setProps({ volume: 70 })
    expect(w.get('[data-volume]').text()).toBe('70 %')
  })

  it('the speaker is the Mute toggle, and tells its state', async () => {
    // Requested from use on the old page: one read "Volume: 60 %" without
    // understanding why nothing came out. Here mute strikes the value through
    // and changes the icon, at the one place where one looks for the sound.
    const w = mounted({ muted: true })
    const button = w.get('[data-remote-command="Mute"]')
    expect(button.attributes('aria-pressed')).toBe('true')
    expect(button.attributes('data-on')).toBe('true')
    expect(w.get('[data-volume]').classes()).toContain('line-through')
    await button.trigger('click')
    expect(w.emitted('mute')).toHaveLength(1)
  })

  it('the gesture follows the finger locally and only commits on release', async () => {
    // A single `set` per gesture: during the drag, only the display moves.
    // Direct emission on the component (as in ProgressBar.test.ts) rather
    // than real pointerdown/move/up, under jsdom without real layout.
    const w = mounted()
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('update:modelValue', [25])
    expect(w.emitted('set')).toBeUndefined()
    expect(w.get('[data-volume]').text()).toBe('25 %')
    await slider.vm.$emit('valueCommit', [25])
    expect(w.emitted('set')).toEqual([[25]])
  })

  it('the target value holds until the frame that reaches it', async () => {
    // Without this, the next frame (volume from before the adjustment)
    // brought the handle back for an instant — the same defect as on
    // ProgressBar.
    const w = mounted()
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('valueCommit', [25])
    expect(w.emitted('set')).toEqual([[25]])
    await w.setProps({ volume: 60 }) // the frame from before the adjustment
    expect(w.get('[data-volume]').text()).toBe('25 %')
    await w.setProps({ volume: 25 }) // the confirming frame
    expect(w.get('[data-volume]').text()).toBe('25 %')
  })

  it('a volume changed elsewhere (infrared remote) releases the target value', async () => {
    // The page commits 40, then someone else (IR remote) touches the volume
    // again: 41, 42, 43... The frame never falls back on the target value (25
    // here), so strict equality alone would leave the display frozen — but any
    // frame different from the previous one proves the device has spoken and
    // must release the target.
    const w = mounted()
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('valueCommit', [25])
    expect(w.emitted('set')).toEqual([[25]])
    // In-flight frame, still the value from before the adjustment: releases nothing.
    await w.setProps({ volume: 60 })
    expect(w.get('[data-volume]').text()).toBe('25 %')
    // The device finally speaks, but not with the target value: it must
    // release anyway, otherwise the page would stay frozen on "25 %" forever.
    await w.setProps({ volume: 61 })
    expect(w.get('[data-volume]').text()).toBe('61 %')
  })

  it('in standby, slider and toggle are greyed out', () => {
    const w = mounted({ disabled: true })
    expect(w.get('[data-remote-command="Mute"]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-slot="slider"]').attributes('data-disabled')).toBeDefined()
  })
})
