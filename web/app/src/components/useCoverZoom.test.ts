import { beforeEach, describe, expect, it } from 'vitest'
import { IDENTITY } from './zoomMath'
import { useCoverZoom, type ZoomMeasure } from './useCoverZoom'

// A landscape cover on a portrait phone, centred on the viewport. At ×2 the
// image is 800 wide, so x may range over [-200, 200]; it is 600 tall, which
// still fits in 800, so y stays 0.
const measure = (): ZoomMeasure => ({
  box: { width: 400, height: 300 },
  viewport: { width: 400, height: 800 },
  center: { x: 200, y: 400 },
})

let t = 0
let zoom: ReturnType<typeof useCoverZoom>

function pe(type: string, id: number, x: number, y: number, pointerType = 'touch', button = 0): PointerEvent {
  return new PointerEvent(type, { pointerId: id, clientX: x, clientY: y, pointerType, button })
}

function down(id: number, x: number, y = 400) {
  zoom.onPointerdown(pe('pointerdown', id, x, y))
}
function move(id: number, x: number, y = 400) {
  zoom.onPointermove(pe('pointermove', id, x, y))
}
function up(id: number, x: number, y = 400) {
  zoom.onPointerup(pe('pointerup', id, x, y))
}

function tap(id: number, x: number, y = 400) {
  down(id, x, y)
  t += 50
  up(id, x, y)
}

// Pinch about the viewport centre to exactly ×2, leaving x at 0, then lift
// both fingers and consume the (negative) decision.
function zoomTo2() {
  down(1, 150)
  down(2, 250)
  move(2, 300)
  move(1, 100)
  up(1, 100)
  up(2, 300)
  expect(zoom.view.value).toEqual({ scale: 2, x: 0, y: 0 })
  expect(zoom.shouldClose()).toBe(false)
}

function wheel(deltaY: number, x = 200, y = 400): WheelEvent {
  const e = new WheelEvent('wheel', { deltaY, clientX: x, clientY: y, cancelable: true })
  zoom.onWheel(e)
  return e
}

beforeEach(() => {
  t = 1000
  zoom = useCoverZoom(measure, () => t)
})

describe('useCoverZoom', () => {
  it('a quick tap at original size asks to close', () => {
    down(1, 200)
    t += 100
    up(1, 200)
    expect(zoom.shouldClose()).toBe(true)
  })

  it('a pinch opens out the image and never asks to close', () => {
    down(1, 150)
    down(2, 250)
    move(2, 350)
    move(1, 50)
    expect(zoom.view.value.scale).toBe(3)
    expect(zoom.gesturing.value).toBe(true)
    up(1, 50)
    up(2, 350)
    expect(zoom.gesturing.value).toBe(false)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('a pinch that ends at original size still does not close', () => {
    down(1, 150)
    down(2, 250)
    move(2, 350)
    move(2, 250)
    expect(zoom.view.value).toEqual(IDENTITY)
    up(2, 250)
    up(1, 150)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('lifting one finger of a pinch continues as a pan without a jump', () => {
    down(1, 150)
    down(2, 250)
    move(2, 350)
    // Midpoint moved from 200 to 250: x = 50 at ×2.
    expect(zoom.view.value).toEqual({ scale: 2, x: 50, y: 0 })
    // Both fingers move during the pinch, so the remaining finger is no longer
    // where it went down: a pan anchored on the press point instead of the
    // current one would jump by 30 px.
    move(1, 120)
    move(2, 320)
    // Still 200 px apart, midpoint 220: x = 20 at ×2.
    expect(zoom.view.value).toEqual({ scale: 2, x: 20, y: 0 })
    up(2, 320)
    const before = zoom.view.value
    move(1, 150)
    expect(zoom.view.value).toEqual({ scale: 2, x: before.x + 30, y: 0 })
    up(1, 150)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('a drag at original size does not close and does not move the image', () => {
    down(1, 200)
    move(1, 260)
    up(1, 260)
    expect(zoom.view.value).toEqual(IDENTITY)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('a drag when zoomed pans, clamped at the edge', () => {
    zoomTo2()
    down(3, 100)
    move(3, 600)
    up(3, 600)
    expect(zoom.view.value).toEqual({ scale: 2, x: 200, y: 0 })
    expect(zoom.shouldClose()).toBe(false)
  })

  it('a mouse moving with no button down does not pan', () => {
    zoomTo2()
    zoom.onPointermove(pe('pointermove', 7, 300, 400, 'mouse'))
    zoom.onPointermove(pe('pointermove', 7, 400, 400, 'mouse'))
    expect(zoom.view.value).toEqual({ scale: 2, x: 0, y: 0 })
    expect(zoom.gesturing.value).toBe(false)
  })

  it('a right mouse press is not a gesture, so hover after it does not pan', () => {
    zoomTo2()
    // A context menu opened on press swallows the release: no pointerup.
    zoom.onPointerdown(pe('pointerdown', 1, 200, 400, 'mouse', 2))
    expect(zoom.gesturing.value).toBe(false)
    zoom.onPointermove(pe('pointermove', 1, 300, 400, 'mouse'))
    expect(zoom.view.value).toEqual({ scale: 2, x: 0, y: 0 })
  })

  it('a press on an id whose release was lost starts afresh', () => {
    // Mouse press at original size whose pointerup never arrives.
    zoom.onPointerdown(pe('pointerdown', 1, 200, 400, 'mouse'))
    t += 5000
    // The next click on the same id is a quick tap at ×1: it closes.
    zoom.onPointerdown(pe('pointerdown', 1, 200, 400, 'mouse'))
    t += 50
    zoom.onPointerup(pe('pointerup', 1, 200, 400, 'mouse'))
    expect(zoom.gesturing.value).toBe(false)
    expect(zoom.shouldClose()).toBe(true)
  })

  it('a lost capture ends the pointer, and hover after it does not pan', () => {
    zoomTo2()
    zoom.onPointerdown(pe('pointerdown', 1, 200, 400, 'mouse'))
    zoom.onLostpointercapture(pe('lostpointercapture', 1, 200, 400, 'mouse'))
    expect(zoom.gesturing.value).toBe(false)
    expect(zoom.shouldClose()).toBe(false)
    zoom.onPointermove(pe('pointermove', 1, 300, 400, 'mouse'))
    expect(zoom.view.value).toEqual({ scale: 2, x: 0, y: 0 })
  })

  it('the lost capture that follows every captured release changes nothing', () => {
    down(1, 200)
    t += 50
    up(1, 200)
    zoom.onLostpointercapture(pe('lostpointercapture', 1, 200, 400))
    expect(zoom.shouldClose()).toBe(true)
  })

  it('a slow press at original size still closes', () => {
    // Before zooming existed every click on the veil closed, however long the
    // press: only a double tap needs a duration, and there is none at ×1.
    down(1, 200)
    t += 400
    up(1, 200)
    expect(zoom.shouldClose()).toBe(true)
  })

  it('a slow press while zoomed neither closes nor starts a double tap', () => {
    zoomTo2()
    down(3, 200)
    t += 400
    up(3, 200)
    expect(zoom.shouldClose()).toBe(false)
    t += 100
    tap(4, 200)
    expect(zoom.view.value.scale).toBe(2)
  })

  it('a cancelled pointer is neither a tap nor a pan, and leaves nothing behind', () => {
    wheel(-Math.log(2) / 0.0015)
    expect(zoom.view.value.scale).toBeCloseTo(2)
    expect(zoom.view.value.x).toBe(0)
    expect(zoom.shouldClose()).toBe(false)

    down(1, 200)
    move(1, 230)
    zoom.onPointercancel(pe('pointercancel', 1, 230, 400))
    expect(zoom.gesturing.value).toBe(false)
    expect(zoom.shouldClose()).toBe(false)
    const before = zoom.view.value

    down(2, 200)
    move(2, 220)
    up(2, 220)
    expect(zoom.view.value).toEqual({ scale: before.scale, x: before.x + 20, y: 0 })
    expect(zoom.shouldClose()).toBe(false)
  })

  it('a single tap while zoomed does not close', () => {
    zoomTo2()
    tap(3, 200)
    expect(zoom.shouldClose()).toBe(false)
    expect(zoom.view.value.scale).toBe(2)
  })

  it('a double tap while zoomed returns to original size', () => {
    zoomTo2()
    tap(3, 200)
    t += 200
    tap(4, 210)
    expect(zoom.view.value).toEqual(IDENTITY)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('two taps too far apart in time or space are not a double tap', () => {
    zoomTo2()
    tap(3, 200)
    t += 400
    tap(4, 200)
    expect(zoom.view.value.scale).toBe(2)

    zoom.reset()
    zoomTo2()
    tap(5, 200)
    t += 100
    tap(6, 250)
    expect(zoom.view.value.scale).toBe(2)
  })

  it('a pan between two taps breaks the double tap', () => {
    zoomTo2()
    tap(3, 200)
    down(4, 200)
    move(4, 240)
    move(4, 200)
    up(4, 200)
    tap(5, 200)
    expect(zoom.view.value.scale).toBe(2)
  })

  it('the wheel zooms around the cursor, within bounds', () => {
    const e = wheel(-500, 300)
    expect(zoom.view.value.scale).toBeCloseTo(Math.exp(0.75))
    // The point under the cursor (100 px right of centre) stays put.
    expect(zoom.view.value.x).toBeCloseTo(100 - 100 * Math.exp(0.75))
    expect(e.defaultPrevented).toBe(true)
    for (let i = 0; i < 20; i++) wheel(-500, 300)
    expect(zoom.view.value.scale).toBe(4)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('the wheel back to original size leaves the close decision alone', () => {
    wheel(-500)
    expect(zoom.shouldClose()).toBe(false)
    wheel(5000)
    expect(zoom.view.value).toEqual(IDENTITY)
    expect(zoom.shouldClose()).toBe(true)
  })

  it('shouldClose defaults to true without a gesture', () => {
    expect(zoom.shouldClose()).toBe(true)
    down(1, 150)
    down(2, 250)
    move(2, 350)
    up(2, 350)
    up(1, 150)
    expect(zoom.shouldClose()).toBe(false)
    expect(zoom.shouldClose()).toBe(true)
  })

  it('reclamp fits the view to a new measure without ending the gesture', () => {
    // The full image arrives while a finger is panning: its box shrinks.
    let box = { width: 400, height: 300 }
    zoom = useCoverZoom(
      () => ({ box, viewport: { width: 400, height: 800 }, center: { x: 200, y: 400 } }),
      () => t,
    )
    zoomTo2()
    down(3, 300)
    move(3, 500)
    expect(zoom.view.value).toEqual({ scale: 2, x: 200, y: 0 })

    // At ×2 the image is now 600 wide: x may only range over [-100, 100].
    box = { width: 300, height: 225 }
    zoom.reclamp()
    expect(zoom.view.value).toEqual({ scale: 2, x: 100, y: 0 })
    expect(zoom.gesturing.value).toBe(true)

    // The finger still down keeps panning, from where the image now is.
    move(3, 450)
    expect(zoom.view.value).toEqual({ scale: 2, x: 50, y: 0 })
    up(3, 450)
    expect(zoom.gesturing.value).toBe(false)
    expect(zoom.shouldClose()).toBe(false)
  })

  it('reset returns to original size', () => {
    zoomTo2()
    zoom.reset()
    expect(zoom.view.value).toEqual(IDENTITY)
    expect(zoom.transform.value).toBe('translate(0px, 0px) scale(1)')
  })

  it('renders the view as a CSS transform', () => {
    down(1, 150)
    down(2, 250)
    move(2, 350)
    expect(zoom.transform.value).toBe('translate(50px, 0px) scale(2)')
  })
})
