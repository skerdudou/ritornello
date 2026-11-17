import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import Slider from './Slider.vue'

// jsdom has neither ResizeObserver (reka-ui's `useSize`) nor pointer capture
// (reka-ui's pointerdown captures the pointer before starting a slide).
beforeAll(() => {
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    unobserve() {}
    disconnect() {}
  })
  Element.prototype.setPointerCapture ??= () => {}
  Element.prototype.hasPointerCapture ??= () => false
  Element.prototype.releasePointerCapture ??= () => {}
})

let wrappers: { unmount: () => void }[] = []
afterEach(() => {
  wrappers.forEach((w) => w.unmount())
  wrappers = []
})

function mountSlider(orientation: 'vertical' | 'horizontal') {
  const w = mount(Slider, {
    props: { modelValue: [0], min: -12, max: 12, step: 1, orientation, 'aria-label': 'Band' },
    attachTo: document.body,
  })
  wrappers.push(w)
  const root = w.get('[data-slot="slider"]').element as HTMLElement
  // A 100 x 100 px slider at the origin: a pointer at (0, 0) is the top of a
  // vertical one (its maximum) and the left of a horizontal one (its minimum)
  // — either way not the current 0, so a started slide shows as a new value.
  root.getBoundingClientRect = () =>
    ({ top: 0, left: 0, right: 100, bottom: 100, width: 100, height: 100, x: 0, y: 0, toJSON() {} }) as DOMRect
  return { w, root }
}

function pointer(type: string, target: Element, pointerType: string, clientY = 0) {
  target.dispatchEvent(new PointerEvent(type, {
    bubbles: true, cancelable: true, pointerId: 1, pointerType, clientX: 0, clientY,
  }))
}
const pointerdown = (target: Element, pointerType: string) => pointer('pointerdown', target, pointerType)

/**
 * On a phone the equalizer's ten vertical sliders cover most of their card:
 * a swipe meant to scroll the page used to start a slide wherever the finger
 * landed and rewrite a gain. On a vertical slider, a finger moves the value
 * by the thumb alone; the mouse, and horizontal sliders, keep drag-to-set on
 * the whole track.
 */
describe('Slider: what a pointerdown on the track starts', () => {
  // Vue's listener wrapper ignores an event stamped no later than its own
  // attachment, and the stamp is taken by the first Vue listener that sees
  // the event. A Vue capture listener on the root stamped it before reka-ui's
  // listener did, so a pointerdown in the same tick as the mount (Vue caches
  // its clock for a tick) was dropped: a progress-bar test missed its seek
  // about once in fifteen runs. The clock is frozen so that the mount and the
  // pointerdown share one millisecond every time, not by chance.
  it('a pointerdown in the millisecond of the mount still reaches reka-ui', async () => {
    const now = vi.spyOn(Date, 'now').mockReturnValue(1_000_000)
    const { w, root } = mountSlider('horizontal')
    pointerdown(root, 'mouse')
    now.mockRestore()
    await flushPromises()
    expect(w.emitted('update:modelValue')?.[0]).toEqual([[-12]])
  })

  it('a touch on the track of a vertical slider changes nothing', async () => {
    const { w } = mountSlider('vertical')
    await flushPromises()
    pointerdown(w.get('[data-slot="slider-track"]').element, 'touch')
    pointerdown(w.get('[data-slot="slider-range"]').element, 'touch')
    await flushPromises()
    expect(w.emitted('update:modelValue')).toBeUndefined()
  })

  it('a touch on the padding of a vertical slider (the root itself) changes nothing', async () => {
    // The event is then at its target, where reka-ui's own listener sits:
    // the capture listener runs first and must keep the bubble pass out.
    const { w, root } = mountSlider('vertical')
    await flushPromises()
    pointerdown(root, 'touch')
    pointerdown(root, 'pen')
    await flushPromises()
    expect(w.emitted('update:modelValue')).toBeUndefined()
  })

  it('a mouse on the track of a vertical slider still sets the value', async () => {
    const { w } = mountSlider('vertical')
    await flushPromises()
    pointerdown(w.get('[data-slot="slider-track"]').element, 'mouse')
    await flushPromises()
    expect(w.emitted('update:modelValue')?.[0]).toEqual([[12]])
  })

  it('a touch on the thumb of a vertical slider still reaches reka-ui', async () => {
    const { w } = mountSlider('vertical')
    await flushPromises()
    const thumb = w.get('[role="slider"]').element as HTMLElement
    pointerdown(thumb, 'touch')
    await flushPromises()
    // reka-ui focuses the thumb it was given, rather than starting a slide.
    expect(document.activeElement).toBe(thumb)
    expect(w.emitted('update:modelValue')).toBeUndefined()
  })

  it('a touch on the track of a horizontal slider still sets the value', async () => {
    const { w } = mountSlider('horizontal')
    await flushPromises()
    pointerdown(w.get('[data-slot="slider-track"]').element, 'touch')
    await flushPromises()
    expect(w.emitted('update:modelValue')?.[0]).toEqual([[-12]])
  })

  /**
   * The browser captures a touch pointer on the element it landed on by
   * itself, so reka-ui's pointermove (which only checks for a capture) would
   * run the rest of a swipe as a slide: measured on a phone, the move before
   * the pan's `pointercancel` left reka-ui holding the slider's pre-scroll
   * rectangle, and the next thumb drag down set +11 dB.
   */
  describe('the rest of a gesture kept away', () => {
    afterEach(() => vi.restoreAllMocks())

    it('the moves of a touch that landed on the track change nothing', async () => {
      vi.spyOn(Element.prototype, 'hasPointerCapture').mockReturnValue(true)
      const { w } = mountSlider('vertical')
      await flushPromises()
      const track = w.get('[data-slot="slider-track"]').element
      pointerdown(track, 'touch')
      pointer('pointermove', track, 'touch', 30)
      pointer('pointermove', track, 'touch', 60)
      pointer('pointerup', track, 'touch', 60)
      await flushPromises()
      expect(w.emitted('update:modelValue')).toBeUndefined()
    })

    it('a touch that landed on the thumb still drags it', async () => {
      vi.spyOn(Element.prototype, 'hasPointerCapture').mockReturnValue(true)
      const { w } = mountSlider('vertical')
      await flushPromises()
      const thumb = w.get('[role="slider"]').element
      pointerdown(thumb, 'touch')
      pointer('pointermove', thumb, 'touch', 30)
      await flushPromises()
      expect(w.emitted('update:modelValue')).toBeDefined()
    })

    it('a gesture kept away ends with its pointercancel: the same pointer id then drags the thumb', async () => {
      // Pointer ids are reused from one touch to the next.
      vi.spyOn(Element.prototype, 'hasPointerCapture').mockReturnValue(true)
      const { w } = mountSlider('vertical')
      await flushPromises()
      const track = w.get('[data-slot="slider-track"]').element
      const thumb = w.get('[role="slider"]').element
      pointerdown(track, 'touch')
      pointer('pointercancel', track, 'touch')
      pointerdown(thumb, 'touch')
      pointer('pointermove', thumb, 'touch', 30)
      await flushPromises()
      expect(w.emitted('update:modelValue')).toBeDefined()
    })

    // Since a new pointerdown frees its id anyway, this one documents the
    // release path rather than guards it: removing the pointerup listener
    // alone keeps it green.
    it('a gesture kept away ends with its pointerup: the same pointer id then drags the thumb', async () => {
      vi.spyOn(Element.prototype, 'hasPointerCapture').mockReturnValue(true)
      const { w } = mountSlider('vertical')
      await flushPromises()
      const track = w.get('[data-slot="slider-track"]').element
      const thumb = w.get('[role="slider"]').element
      pointerdown(track, 'touch')
      pointer('pointerup', track, 'touch')
      pointerdown(thumb, 'touch')
      pointer('pointermove', thumb, 'touch', 30)
      await flushPromises()
      expect(w.emitted('update:modelValue')).toBeDefined()
    })

    it('a gesture whose end never reached the root does not block the next drag of the same id', async () => {
      // A pen lifted off the slider, with no implicit capture: neither its
      // pointerup nor a pointercancel reaches the root.
      vi.spyOn(Element.prototype, 'hasPointerCapture').mockReturnValue(true)
      const { w } = mountSlider('vertical')
      await flushPromises()
      const track = w.get('[data-slot="slider-track"]').element
      const thumb = w.get('[role="slider"]').element
      pointerdown(track, 'pen')
      pointerdown(thumb, 'pen')
      pointer('pointermove', thumb, 'pen', 30)
      await flushPromises()
      expect(w.emitted('update:modelValue')).toBeDefined()
    })
  })
})
