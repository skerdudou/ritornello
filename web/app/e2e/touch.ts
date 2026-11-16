import type { Page } from '@playwright/test'

export interface Point {
  x: number
  y: number
}

/**
 * A real finger drag, through the Chrome DevTools Protocol.
 *
 * Playwright's `page.touchscreen` only taps, and `page.mouse` is a mouse
 * pointer: neither is a finger moving across the screen. CDP's
 * `Input.dispatchTouchEvent` goes through Chromium's own input pipeline, so
 * the page receives what a phone would give it — touch and pointer events of
 * a touch pointer, with `touch-action` honoured and the browser scrolling the
 * page itself when the gesture is left to it. That is the whole point of the
 * journeys using it: a handle that lets the page steal the gesture, or a row
 * that does not, only shows up here.
 *
 * `touchStart` at `from`, `steps` interpolated `touchMove`s 16 ms apart (one
 * per frame, so a drag library sees a continuous gesture), then, while
 * `holdMs` lasts, the finger stays at `to` and keeps reporting it every 50 ms
 * — as a real finger resting on glass does — and finally `touchEnd`.
 * Coordinates are CSS pixels in the viewport, as `boundingBox()` gives them.
 */
export async function touchDrag(
  page: Page,
  from: Point,
  to: Point,
  opts: { steps?: number; holdMs?: number } = {},
): Promise<void> {
  const steps = opts.steps ?? 12
  const holdMs = opts.holdMs ?? 0
  const cdp = await page.context().newCDPSession(page)
  try {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [from] })
    for (let i = 1; i <= steps; i++) {
      const point = {
        x: from.x + ((to.x - from.x) * i) / steps,
        y: from.y + ((to.y - from.y) * i) / steps,
      }
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [point] })
      await page.waitForTimeout(16)
    }
    for (let held = 0; held < holdMs; held += 50) {
      await page.waitForTimeout(50)
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [to] })
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
  } finally {
    await cdp.detach()
  }
}
