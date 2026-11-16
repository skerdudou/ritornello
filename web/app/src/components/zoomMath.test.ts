import { describe, expect, it } from 'vitest'
import { IDENTITY, clampView, pan, pinch, zoomAround } from './zoomMath'

// A landscape cover on a portrait phone: wider than tall, viewport taller than the box.
const box = { width: 400, height: 300 }
const viewport = { width: 400, height: 800 }

describe('clampView', () => {
  it('leaves nothing to pan at scale 1', () => {
    expect(clampView({ scale: 1, x: 50, y: 50 }, box, viewport)).toEqual({ scale: 1, x: 0, y: 0 })
  })

  it('bounds x by the overflow and keeps a short axis centred', () => {
    // x: (800 - 400) / 2 = 200; y: 600 < 800, so it stays centred.
    expect(clampView({ scale: 2, x: 500, y: 500 }, box, viewport)).toEqual({ scale: 2, x: 200, y: 0 })
  })

  it('bounds both axes when both overflow', () => {
    // x: (1600 - 400) / 2 = 600; y: (1200 - 800) / 2 = 200.
    expect(clampView({ scale: 4, x: -999, y: 999 }, box, viewport)).toEqual({ scale: 4, x: -600, y: 200 })
  })

  it('clamps the scale into [1, 4]', () => {
    expect(clampView({ scale: 9, x: 0, y: 0 }, box, viewport).scale).toBe(4)
    expect(clampView({ scale: 0.3, x: 0, y: 0 }, box, viewport).scale).toBe(1)
  })

  it('never yields NaN or -0 when nothing is laid out', () => {
    expect(clampView({ scale: 2, x: 10, y: 10 }, { width: 0, height: 0 }, { width: 0, height: 0 })).toEqual({
      scale: 2,
      x: 0,
      y: 0,
    })
    // -0 would survive a naive Math.max(-m, Math.min(m, v)) when m is 0.
    expect(Object.is(clampView({ scale: 1, x: -5, y: -5 }, box, viewport).x, 0)).toBe(true)
    expect(Object.is(clampView({ scale: 1, x: -5, y: -5 }, box, viewport).y, 0)).toBe(true)
  })
})

describe('zoomAround', () => {
  it('zooms about the centre without moving', () => {
    expect(zoomAround(IDENTITY, 2, { x: 0, y: 0 }, box, viewport)).toEqual({ scale: 2, x: 0, y: 0 })
  })

  it('keeps the point under the finger under it', () => {
    expect(zoomAround(IDENTITY, 2, { x: 100, y: 0 }, box, viewport)).toEqual({ scale: 2, x: -100, y: 0 })
  })

  it('re-clamps when zooming out, leaving no gap', () => {
    expect(zoomAround({ scale: 2, x: -100, y: 0 }, 0.5, { x: 100, y: 0 }, box, viewport)).toEqual({
      scale: 1,
      x: 0,
      y: 0,
    })
  })

  it('caps the scale at 4', () => {
    expect(zoomAround(IDENTITY, 10, { x: 0, y: 0 }, box, viewport).scale).toBe(4)
  })

  it('does not move when already at the cap', () => {
    const at = { scale: 4, x: 0, y: 0 }
    expect(zoomAround(at, 2, { x: 100, y: 50 }, box, viewport)).toEqual(at)
  })
})

describe('pinch', () => {
  it('scales by the ratio of finger distances', () => {
    expect(pinch(IDENTITY, { x: -50, y: 0 }, { x: 50, y: 0 }, { x: -100, y: 0 }, { x: 100, y: 0 }, box, viewport)).toEqual({
      scale: 2,
      x: 0,
      y: 0,
    })
  })

  it('follows the midpoint', () => {
    const v = pinch(IDENTITY, { x: -50, y: 0 }, { x: 50, y: 0 }, { x: -50, y: 0 }, { x: 150, y: 0 }, box, viewport)
    expect(v.scale).toBe(2)
    expect(v.x).toBe(50)
  })

  it('survives coincident starting fingers', () => {
    const p = { x: 10, y: 10 }
    const v = pinch(IDENTITY, p, p, { x: 0, y: 0 }, { x: 100, y: 0 }, box, viewport)
    expect(v.scale).toBe(1)
    expect(Number.isNaN(v.x) || Number.isNaN(v.y)).toBe(false)
  })
})

describe('pan', () => {
  it('adds the delta', () => {
    expect(pan({ scale: 2, x: 0, y: 0 }, 150, 50, box, viewport)).toEqual({ scale: 2, x: 150, y: 0 })
  })

  it('stops at the edge', () => {
    expect(pan({ scale: 2, x: 0, y: 0 }, 300, 0, box, viewport).x).toBe(200)
  })
})
