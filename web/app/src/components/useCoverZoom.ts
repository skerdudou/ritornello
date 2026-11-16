// Gesture state machine for the enlarged cover: pinch to zoom, drag to pan
// while zoomed, double tap to return to original size, wheel to zoom with a
// mouse. The arithmetic lives in `zoomMath`; this file only decides which
// operation a stream of pointer events means.
//
// Closing. The overlay closes on `click`, and the browser fires that `click`
// after `pointerup`, on the overlay, whatever the pointers did before: the end
// of a pinch or of a pan would close the image the user was looking at. At
// that point only this state machine knows whether the sequence that just
// ended was a plain tap at original size, so it takes the decision at the last
// `pointerup` and the click handler consumes it with `shouldClose()`. The
// decision then falls back to `true`, so a click with no pointer sequence
// before it (keyboard activation, a test calling `.click()`) still closes.
//
// At original size a single tap closes at once, without waiting to see
// whether a second tap follows: a double tap only means something while
// zoomed, where it returns to ×1. Pinching is the only way to zoom in on touch.

import { computed, ref, shallowRef, type ComputedRef, type Ref } from 'vue'
import { IDENTITY, pan, pinch, zoomAround, type Point, type Size, type View } from './zoomMath'

export interface ZoomMeasure {
  box: Size
  viewport: Size
  /** The viewport centre, in client coordinates. */
  center: { x: number; y: number }
}

export const TAP_SLOP_PX = 10
export const TAP_MAX_MS = 300
export const DOUBLE_TAP_MS = 300
export const DOUBLE_TAP_PX = 30

// Wheel zoom factor per pixel of deltaY: exp(-deltaY * k).
const WHEEL_RATE = 0.0015

interface Anchor {
  view: View
  points: Map<number, Point>
}

interface Gesture {
  startedAt: number
  scaleAtStart: number
  maxPointers: number
  maxTravel: number
  cancelled: boolean
}

export function useCoverZoom(
  measure: () => ZoomMeasure,
  now: () => number = () => performance.now(),
): {
  view: Readonly<Ref<View>>
  gesturing: Readonly<Ref<boolean>>
  transform: ComputedRef<string>
  onPointerdown(e: PointerEvent): void
  onPointermove(e: PointerEvent): void
  onPointerup(e: PointerEvent): void
  onPointercancel(e: PointerEvent): void
  onLostpointercapture(e: PointerEvent): void
  onWheel(e: WheelEvent): void
  shouldClose(): boolean
  reset(): void
} {
  const view = shallowRef<View>(IDENTITY)
  const gesturing = ref(false)
  const transform = computed(() => {
    const v = view.value
    return `translate(${v.x}px, ${v.y}px) scale(${v.scale})`
  })

  // Pointers currently down, in client coordinates, and where each went down.
  // A pointer absent from here (a mouse hovering with no button down, or a
  // pointer that went down elsewhere) is ignored by move, up and cancel.
  const pointers = new Map<number, Point>()
  const downAt = new Map<number, Point>()
  // The view and pointer positions every move is computed from. Taken again
  // whenever the number of pointers changes, so lifting one finger of a pinch
  // continues as a pan from where the image is, without a jump.
  let anchor: Anchor = { view: IDENTITY, points: new Map() }
  let gesture: Gesture | null = null
  let closeDecision = true
  let lastTap: { at: number; point: Point } | null = null

  function reanchor() {
    anchor = { view: view.value, points: new Map(pointers) }
  }

  function centred(p: Point, m: ZoomMeasure): Point {
    return { x: p.x - m.center.x, y: p.y - m.center.y }
  }

  function onPointerdown(e: PointerEvent) {
    // Only the primary mouse button gestures. A right press may open a
    // context menu that swallows its release, leaving a pointer that never
    // comes up.
    if (e.pointerType === 'mouse' && e.button !== 0) return
    // A down on an id still tracked means its up was lost (a mouse always has
    // the same id). Forget the stale pointer and start afresh, rather than
    // ignoring this press and letting its up end the stale gesture.
    if (pointers.has(e.pointerId)) {
      pointers.delete(e.pointerId)
      downAt.delete(e.pointerId)
      if (pointers.size === 0) gesture = null
    }
    try {
      ;(e.currentTarget as Element | null)?.setPointerCapture?.(e.pointerId)
    } catch {
      // Capture is a convenience: without it a finger leaving the overlay
      // ends with a cancel or an up elsewhere, which is handled the same.
    }
    const p = { x: e.clientX, y: e.clientY }
    if (pointers.size === 0) {
      gesture = { startedAt: now(), scaleAtStart: view.value.scale, maxPointers: 0, maxTravel: 0, cancelled: false }
    }
    pointers.set(e.pointerId, p)
    downAt.set(e.pointerId, p)
    if (gesture) gesture.maxPointers = Math.max(gesture.maxPointers, pointers.size)
    gesturing.value = true
    reanchor()
  }

  function onPointermove(e: PointerEvent) {
    if (!pointers.has(e.pointerId)) return
    const p = { x: e.clientX, y: e.clientY }
    pointers.set(e.pointerId, p)
    const origin = downAt.get(e.pointerId)!
    if (gesture) gesture.maxTravel = Math.max(gesture.maxTravel, Math.hypot(p.x - origin.x, p.y - origin.y))

    const m = measure()
    if (pointers.size === 1) {
      if (anchor.view.scale <= 1) return
      const from = anchor.points.get(e.pointerId)
      if (!from) return
      view.value = pan(anchor.view, p.x - from.x, p.y - from.y, m.box, m.viewport)
      return
    }
    // Two pointers or more: pinch with the first two; a third is ignored.
    const [ia, ib] = [...anchor.points.keys()]
    const a0 = anchor.points.get(ia!)
    const b0 = anchor.points.get(ib!)
    const a = pointers.get(ia!)
    const b = pointers.get(ib!)
    if (!a0 || !b0 || !a || !b) return
    view.value = pinch(
      anchor.view,
      centred(a0, m),
      centred(b0, m),
      centred(a, m),
      centred(b, m),
      m.box,
      m.viewport,
    )
  }

  function release(e: PointerEvent, cancelled: boolean) {
    if (!pointers.has(e.pointerId)) return
    pointers.delete(e.pointerId)
    downAt.delete(e.pointerId)
    if (gesture && cancelled) gesture.cancelled = true
    reanchor()
    if (pointers.size > 0) return
    gesturing.value = false
    const g = gesture
    gesture = null
    if (!g) return
    finish(g, { x: e.clientX, y: e.clientY })
  }

  function finish(g: Gesture, point: Point) {
    const at = now()
    const tap =
      !g.cancelled && g.maxPointers === 1 && g.maxTravel <= TAP_SLOP_PX && at - g.startedAt <= TAP_MAX_MS
    if (tap && g.scaleAtStart > 1) {
      const completes =
        lastTap !== null &&
        at - lastTap.at <= DOUBLE_TAP_MS &&
        Math.hypot(point.x - lastTap.point.x, point.y - lastTap.point.y) <= DOUBLE_TAP_PX
      if (completes) {
        lastTap = null
        view.value = IDENTITY
        reanchor()
      } else {
        lastTap = { at, point }
      }
    } else {
      lastTap = null
    }
    // Taken after a double tap has reset the view: a tap that started zoomed
    // never closes, even though the view is now at ×1.
    closeDecision = tap && view.value.scale === 1 && g.scaleAtStart === 1
  }

  function onPointerup(e: PointerEvent) {
    release(e, false)
  }

  function onPointercancel(e: PointerEvent) {
    release(e, true)
  }

  // Browsers fire `lostpointercapture` after every captured `pointerup`; by
  // then the pointer is no longer tracked and this does nothing. Arriving
  // while the pointer is still tracked (the element re-rendered, capture was
  // taken away), it means no up will come: end the pointer as a cancel.
  function onLostpointercapture(e: PointerEvent) {
    release(e, true)
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault()
    const m = measure()
    const focal = centred({ x: e.clientX, y: e.clientY }, m)
    view.value = zoomAround(view.value, Math.exp(-e.deltaY * WHEEL_RATE), focal, m.box, m.viewport)
    if (pointers.size > 0) reanchor()
    if (view.value.scale > 1) closeDecision = false
  }

  function shouldClose(): boolean {
    const decision = closeDecision
    closeDecision = true
    return decision
  }

  function reset() {
    view.value = IDENTITY
    pointers.clear()
    downAt.clear()
    gesture = null
    gesturing.value = false
    lastTap = null
    closeDecision = true
    reanchor()
  }

  return {
    view,
    gesturing,
    transform,
    onPointerdown,
    onPointermove,
    onPointerup,
    onPointercancel,
    onLostpointercapture,
    onWheel,
    shouldClose,
    reset,
  }
}
