// Pure arithmetic for pinch-zoom and pan on the enlarged cover.
//
// Coordinate convention: the image is centred in the viewport and scaled
// about its centre (CSS `transform-origin: center`). A `Point` is measured
// from the viewport centre, in pixels, y down. A `View` is the scale plus the
// translation (x, y) applied after scaling, in the same space. `box` is the
// image's laid-out size at scale 1; `viewport` is the visible area.
//
// Invariant kept by `clampView`: the scaled image never leaves a gap on an
// axis where it is larger than the viewport, and stays centred on an axis
// where it is smaller.

export interface Size {
  width: number
  height: number
}
export interface Point {
  x: number
  y: number
}
export interface View {
  scale: number
  x: number
  y: number
}

export const MIN_SCALE = 1
export const MAX_SCALE = 4
export const IDENTITY: View = { scale: 1, x: 0, y: 0 }

function clampScale(scale: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale))
}

// Largest allowed |offset| on one axis. Zero when the image fits (or when
// nothing is laid out yet, so a zero-sized box never produces NaN).
function limit(boxLength: number, scale: number, viewportLength: number): number {
  return Math.max(0, (boxLength * scale - viewportLength) / 2)
}

function bound(value: number, max: number): number {
  // `+ 0` turns -0 into 0, so equality checks and CSS stay clean.
  return Math.max(-max, Math.min(max, value)) + 0
}

export function clampView(view: View, box: Size, viewport: Size): View {
  const scale = clampScale(view.scale)
  return {
    scale,
    x: bound(view.x, limit(box.width, scale, viewport.width)),
    y: bound(view.y, limit(box.height, scale, viewport.height)),
  }
}

export function zoomAround(view: View, factor: number, focal: Point, box: Size, viewport: Size): View {
  const scale = clampScale(view.scale * factor)
  const k = scale / view.scale
  return clampView(
    { scale, x: focal.x - (focal.x - view.x) * k, y: focal.y - (focal.y - view.y) * k },
    box,
    viewport,
  )
}

export function pinch(start: View, a0: Point, b0: Point, a: Point, b: Point, box: Size, viewport: Size): View {
  const d0 = Math.hypot(a0.x - b0.x, a0.y - b0.y)
  const factor = d0 === 0 ? 1 : Math.hypot(a.x - b.x, a.y - b.y) / d0
  const scale = clampScale(start.scale * factor)
  const k = scale / start.scale
  const m0 = { x: (a0.x + b0.x) / 2, y: (a0.y + b0.y) / 2 }
  const m = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }
  return clampView(
    { scale, x: m.x - (m0.x - start.x) * k, y: m.y - (m0.y - start.y) * k },
    box,
    viewport,
  )
}

export function pan(start: View, dx: number, dy: number, box: Size, viewport: Size): View {
  return clampView({ scale: start.scale, x: start.x + dx, y: start.y + dy }, box, viewport)
}
