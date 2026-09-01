import { Slider } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import Volume from './Volume.vue'

const mounted = (props: Record<string, unknown> = {}) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Volume, { props: { volume: 60, muted: false, disabled: false, ...props } })
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

  it('commits an absolute setting on release, once only', async () => {
    const w = mounted()
    const handle = w.get('[role="slider"]')
    ;(handle.element as HTMLElement).focus()
    await handle.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('set')).toEqual([[61]])
    expect(w.get('[data-volume]').text()).toBe('61 %')
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
