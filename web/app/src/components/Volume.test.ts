import { Slider } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
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

// Pointer capture and ResizeObserver, which jsdom lacks, for both suites.
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

describe('Volume', () => {
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

// Direct emission on the component (as in ProgressBar.test.ts) rather than
// real pointerdown/move/up, under jsdom without real layout. Every mount
// starts at 60.
describe('Volume — dragging', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  const drag = async (w: ReturnType<typeof mounted>, v: number) => {
    await w.getComponent(Slider).vm.$emit('update:modelValue', [v])
  }
  const release = async (w: ReturnType<typeof mounted>, v: number) => {
    await w.getComponent(Slider).vm.$emit('valueCommit', [v])
  }
  const frame = async (w: ReturnType<typeof mounted>, volume: number) => {
    await w.setProps({ volume })
  }
  const shown = (w: ReturnType<typeof mounted>) => w.get('[data-volume]').text()

  it('the first value of a drag goes at once, then one every 150 ms, the latest kept', async () => {
    const w = mounted()
    await drag(w, 55)
    expect(w.emitted('set')).toEqual([[55]])
    vi.advanceTimersByTime(50)
    await drag(w, 50)
    await drag(w, 45)
    expect(w.emitted('set')).toEqual([[55]])
    vi.advanceTimersByTime(100)
    // The trailing send carries the finger's latest value, not 50 which armed it.
    expect(w.emitted('set')).toEqual([[55], [45]])
  })

  it('the release sends the final value at once and cancels the pending one', async () => {
    const w = mounted()
    await drag(w, 55)
    vi.advanceTimersByTime(10)
    await drag(w, 50)
    await release(w, 50)
    expect(w.emitted('set')).toEqual([[55], [50]])
    vi.advanceTimersByTime(500)
    expect(w.emitted('set')).toEqual([[55], [50]])
  })

  it('a release on the value last sent sends nothing more', async () => {
    const w = mounted()
    await drag(w, 55)
    vi.advanceTimersByTime(200)
    await drag(w, 50)
    expect(w.emitted('set')).toEqual([[55], [50]])
    await release(w, 50)
    expect(w.emitted('set')).toEqual([[55], [50]])
  })

  it('the thumb follows the finger, not the echoes, while dragging', async () => {
    const w = mounted()
    await drag(w, 55)
    vi.advanceTimersByTime(200)
    await drag(w, 45)
    await frame(w, 55)
    expect(shown(w)).toBe('45 %')
    await drag(w, 40)
    await frame(w, 45)
    expect(shown(w)).toBe('40 %')
  })

  it('late echoes of intermediate values do not pull the thumb back', async () => {
    // Review focus: sends 50, 40, 30, then their echoes after the release.
    // The old rule ("any frame different from the previous one releases")
    // took the echo of 50 for another source and dropped the thumb to 50.
    const w = mounted()
    await drag(w, 50)
    vi.advanceTimersByTime(200)
    await drag(w, 40)
    vi.advanceTimersByTime(200)
    await drag(w, 30)
    await release(w, 30)
    await frame(w, 50)
    expect(shown(w)).toBe('30 %')
    await frame(w, 40)
    expect(shown(w)).toBe('30 %')
    await frame(w, 30)
    expect(shown(w)).toBe('30 %')
    // Released by the final echo: a remote press now shows.
    await frame(w, 35)
    expect(shown(w)).toBe('35 %')
  })

  it('an early echo equal to the final value does not release the latch', async () => {
    // Review focus: a back-and-forth sends 50, 45, 50. The first echo of 50
    // is not the final one: releasing on it (`v === target`, or matching the
    // last occurrence in the queue) would let the echo of 45 drop the thumb.
    const w = mounted()
    await drag(w, 50)
    vi.advanceTimersByTime(200)
    await drag(w, 45)
    vi.advanceTimersByTime(200)
    await drag(w, 50)
    await release(w, 50)
    expect(w.emitted('set')).toEqual([[50], [45], [50]])
    await frame(w, 50)
    expect(shown(w)).toBe('50 %')
    await frame(w, 45)
    expect(shown(w)).toBe('50 %')
    await frame(w, 50)
    expect(shown(w)).toBe('50 %')
    await frame(w, 55)
    expect(shown(w)).toBe('55 %')
  })

  it('a coalesced stream that skips echoes still releases on the final one', async () => {
    // The core may coalesce frames: only the echo of 30 arrives. It confirms
    // every older send with it, so the queue is empty and 40 afterwards
    // can only come from another source.
    const w = mounted()
    await drag(w, 50)
    vi.advanceTimersByTime(200)
    await drag(w, 40)
    vi.advanceTimersByTime(200)
    await drag(w, 30)
    await release(w, 30)
    await frame(w, 30)
    expect(shown(w)).toBe('30 %')
    await frame(w, 40)
    expect(shown(w)).toBe('40 %')
  })

  it('a frame from another source releases the latch at once', async () => {
    const w = mounted()
    await drag(w, 50)
    vi.advanceTimersByTime(200)
    await drag(w, 40)
    await release(w, 40)
    await frame(w, 50)
    expect(shown(w)).toBe('40 %')
    await frame(w, 63)
    expect(shown(w)).toBe('63 %')
  })

  it('the latch gives up after 2 s without its echo', async () => {
    const w = mounted()
    await drag(w, 50)
    await release(w, 50)
    vi.advanceTimersByTime(1999)
    await w.vm.$nextTick()
    expect(shown(w)).toBe('50 %')
    vi.advanceTimersByTime(1)
    await w.vm.$nextTick()
    expect(shown(w)).toBe('60 %')
  })

  it('a drag back to its starting value still sends it and lets go of the finger', async () => {
    // Review focus: reka emits no `valueCommit` when a drag ends on the value
    // it started from, so the release is also read from `pointerup`.
    // `hasPointerCapture` is stubbed to true, so reka's own `pointerup` may
    // emit a `valueCommit` too: `finish` is idempotent either way.
    const w = mounted()
    await drag(w, 55)
    vi.advanceTimersByTime(50)
    await drag(w, 60)
    await w.get('[data-volume-slider]').trigger('pointerup')
    expect(w.emitted('set')).toEqual([[55], [60]])
    expect(shown(w)).toBe('60 %')
    await frame(w, 55)
    expect(shown(w)).toBe('60 %')
    await frame(w, 60)
    expect(shown(w)).toBe('60 %')
    await frame(w, 70)
    expect(shown(w)).toBe('70 %')
  })

  it('pointercancel ends the gesture like pointerup', async () => {
    const w = mounted()
    await drag(w, 55)
    await w.get('[data-volume-slider]').trigger('pointercancel')
    await frame(w, 55)
    await frame(w, 70)
    expect(shown(w)).toBe('70 %')
  })

  it('a press and release without movement sends nothing and holds nothing', async () => {
    const w = mounted()
    await w.get('[data-volume-slider]').trigger('pointerup')
    expect(w.emitted('set')).toBeUndefined()
    await frame(w, 70)
    expect(shown(w)).toBe('70 %')
  })

  it('a tap on a value already seen (remote moved since) is sent', async () => {
    // Dedup is per gesture: a `lastSent` kept from the previous gesture would
    // swallow this 50, which the remote has since left.
    const w = mounted()
    await drag(w, 50)
    await release(w, 50)
    await frame(w, 50)
    await frame(w, 55) // the remote
    await drag(w, 50)
    await release(w, 50)
    expect(w.emitted('set')?.slice(-2)).toEqual([[50], [50]])
  })

  it('unmounting cancels a pending send', async () => {
    // A listener spy, since test-utils forgets `emitted()` on unmount. Vue
    // also drops an emit from an unmounted instance, so the spy alone cannot
    // see a timer left behind: the timer count does.
    const onSet = vi.fn()
    const w = mounted({ onSet })
    await drag(w, 55)
    vi.advanceTimersByTime(10)
    await drag(w, 50)
    w.unmount()
    expect(vi.getTimerCount()).toBe(0)
    vi.advanceTimersByTime(500)
    expect(onSet.mock.calls).toEqual([[55]])
    // The latch's backstop too.
    const w2 = mounted()
    await drag(w2, 50)
    await release(w2, 50)
    w2.unmount()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('a release with nothing unconfirmed shows the device at once', async () => {
    // The echo of the last send arrived during the drag, then another source
    // moved the volume: the release sends nothing (the finger is on the value
    // last sent), so there is nothing to hold — the device's 70 shows, not a
    // stale 55 for two seconds.
    const w = mounted()
    await drag(w, 55)
    await frame(w, 55)
    await frame(w, 70)
    expect(shown(w)).toBe('55 %')
    await release(w, 55)
    expect(w.emitted('set')).toEqual([[55]])
    expect(shown(w)).toBe('70 %')
  })
})
