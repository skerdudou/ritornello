import { Slider } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it } from 'vitest'
import ProgressBar from './ProgressBar.vue'

const mounted = (props: Record<string, unknown>) =>
  mount(ProgressBar, { props: { position: 87, duration: 254, seekable: false, step: 10, ...props } })

describe('ProgressBar', () => {
  it('shows the position and the duration', () => {
    const w = mounted({})
    expect(w.get('[data-position]').text()).toBe('1:27')
    expect(w.get('[data-total-duration]').text()).toBe('4:14')
  })

  // An endless bar teaches nothing: without a duration, only the elapsed time is shown.
  it('without a duration, no bar', () => {
    const w = mounted({ duration: null })
    expect(w.find('[data-bar]').exists()).toBe(false)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('fills the bar proportionally', () => {
    const w = mounted({})
    const style = w.get('[data-fill]').attributes('style') ?? ''
    const percent = Number(/width:\s*([\d.]+)%/.exec(style)?.[1])
    // 87 / 254 = 34.25 %. A value read and compared, rather than a substring
    // "34" that would pass just as well on "3.4" or "340".
    expect(percent).toBeCloseTo(34.25, 1)
  })

  // It is `seekable` that decides, not the presence of a duration: Radio
  // France announces a duration on a live stream that cannot be rewound.
  it('inert when the content is not seekable', async () => {
    const w = mounted({ seekable: false })
    await w.get('[data-bar]').trigger('click')
    expect(w.emitted('seek')).toBeUndefined()
    expect(w.get('[data-bar]').attributes('role')).toBeUndefined()
    expect(w.find('[role="slider"]').exists()).toBe(false)
    // The static bar is not a target: it must not pay for the 44 px touch
    // area (`py-[19px]`) reserved for the real slider, and must share the
    // exact same geometry as the slider (`py-0` on both sides, Playwright
    // measurement in support: radio and file must line up).
    const classes = w.get('[data-bar]').classes()
    expect(classes).not.toContain('py-[19px]')
    expect(classes).toContain('py-0')
  })

  // Whatever the state (static bar or slider): the durations line stays below
  // the track in the DOM, never before. A possible regression with negative
  // margins (`-my-[19px]`, `-mt-4`) that could wrongly overlap or reorder the
  // blocks visually.
  it('the durations line stays below the track, non seekable', () => {
    const w = mounted({ seekable: false })
    const track = w.get('[data-bar]').element
    const durations = w.get('[data-position]').element.closest('div')!
    expect(track.compareDocumentPosition(durations) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  })

  it('the durations line stays below the track, seekable', async () => {
    const w = mounted({ seekable: true })
    await flushPromises()
    const track = w.get('[data-slot="slider"]').element
    const durations = w.get('[data-position]').element.closest('div')!
    expect(track.compareDocumentPosition(durations) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  })

  // reka-ui captures the pointer during the drag; jsdom does not implement
  // that API. Three shims, for the duration of the file.
  beforeAll(() => {
    Element.prototype.setPointerCapture ??= () => {}
    Element.prototype.releasePointerCapture ??= () => {}
    Element.prototype.hasPointerCapture ??= () => true
    // jsdom does not provide ResizeObserver; reka-ui uses it to measure the
    // slider track at mount time (see web/kit/src/index.test.ts).
    globalThis.ResizeObserver ??= class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  })

  function rectangle(w: ReturnType<typeof mounted>) {
    const track = w.get('[data-slot="slider"]')
    track.element.getBoundingClientRect = () =>
      ({ left: 0, width: 200, top: 0, height: 44, right: 200, bottom: 44, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect
    return track
  }

  it('seekable content renders an accessible handle', async () => {
    // `await flushPromises()`: reka-ui resolves the handle's index from the
    // collection of mounted thumbs (`SliderThumb`), filled when the DOM ref
    // attaches. On the first render the index is -1 (handle not yet in the
    // collection) and `aria-valuenow` stays absent; one tick later it is 0
    // and the attribute appears. Observed here, same cause as the
    // ResizeObserver stub above (deferred measurement/collection under jsdom).
    const w = mounted({ seekable: true })
    await flushPromises()
    const handle = w.get('[role="slider"]')
    expect(handle.attributes('aria-valuenow')).toBe('87')
    expect(handle.attributes('aria-valuemax')).toBe('254')
  })

  it('the drag follows the finger locally and only commits on release', async () => {
    // A single `SeekTo` per gesture: during the drag, only the display moves.
    //
    // Fallback on the component's events rather than real
    // pointerdown/move/up: under jsdom, `thumb.clientWidth` is always 0 (no
    // real layout), so reka computes the gesture on the full track width
    // (200 px). 150/200 x 254 lands exactly on 190.5 s, which reka's
    // `Math.round` rounds to 191 and not 190 — a measurement disagreement
    // specific to jsdom, not a defect of the component (verified by inspecting
    // `SliderHorizontal.getValueFromPointerEvent`). In a real browser, the
    // handle has a non-zero width and does not land on this boundary; the real
    // gesture is covered by the e2e (Task 12).
    const w = mounted({ seekable: true })
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('update:modelValue', [190])
    expect(w.emitted('seek')).toBeUndefined()
    expect(w.get('[data-position]').text()).toBe('3:10') // 150/200 × 254 = 190 s, displayed during the gesture
    await slider.vm.$emit('valueCommit', [190])
    expect(w.emitted('seek')).toEqual([[190]])
  })

  it('the target value holds until the frame that reaches it', async () => {
    // Without this, the next frame (position from before the jump) brought
    // the handle back for an instant — the visible defect of naive players.
    const w = mounted({ seekable: true })
    const track = rectangle(w)
    await track.trigger('pointerdown', { clientX: 100, pointerId: 1, button: 0 })
    await track.trigger('pointerup', { clientX: 100, pointerId: 1 })
    expect(w.emitted('seek')).toEqual([[127]])
    await w.setProps({ position: 88 }) // the frame from before the jump
    expect(w.get('[data-position]').text()).toBe('2:07')
    await w.setProps({ position: 129 }) // within one step: we reach it
    expect(w.get('[data-position]').text()).toBe('2:09')
  })

  it('a frame without position releases the target value instead of freezing it', async () => {
    // End of track, Stop, standby, source change: none of these frames carries
    // a position, and none will ever confirm the jump — otherwise the bar
    // would stay stuck on the old target forever.
    const w = mounted({ seekable: true, position: 87 })
    const track = rectangle(w)
    await track.trigger('pointerdown', { clientX: 100, pointerId: 1, button: 0 })
    await track.trigger('pointerup', { clientX: 100, pointerId: 1 })
    expect(w.emitted('seek')).toEqual([[127]])
    await w.setProps({ position: null })
    // No more position: nothing is rendered (see the test "shows the position
    // and the duration"), so the target value must leave no trace.
    expect(w.find('[data-progress]').exists()).toBe(false)
    // A next track restarting at 0:01 proves it: without the release, the
    // target (127) would still have masked this brand-new value.
    await w.setProps({ position: 1 })
    expect(w.get('[data-position]').text()).toBe('0:01')
  })

  // Without the keyboard, the bar would be the only control of the page out of
  // reach without a mouse. The step is the physical keys' (`seek_step_s`), not
  // the slider's one second.
  it('the keyboard moves by the configured step, bounded at both ends', async () => {
    const w = mounted({ seekable: true, position: 250 })
    const handle = w.get('[role="slider"]')
    await handle.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('seek')?.[0]).toEqual([254])
    await handle.trigger('keydown', { key: 'Home' })
    expect(w.emitted('seek')?.[1]).toEqual([0])
    await handle.trigger('keydown', { key: 'ArrowLeft' })
    expect(w.emitted('seek')?.[2]).toEqual([240])
  })
})
