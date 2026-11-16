import { expect, test } from '@playwright/test'
import { sidewaysCulprits } from './overflow'
import { touchDrag } from './touch'

test('on a phone: bottom bar, top nav absent, named tile', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-bottom-nav]')).toBeVisible()
  await expect(page.locator('[data-top-nav]')).toBeHidden()
  await expect(page.locator('[data-bottom-nav] a')).toHaveCount(4)
  // Preset 1 (FIP, the harness's stations.toml) is playing and carries its name.
  const tile = page.locator('[data-preset-button="1"]')
  await expect(tile).toHaveAttribute('aria-current', 'true')
  await expect(tile.locator('[data-preset-name]')).toHaveText('FIP')
  // The transport has neither ±10 s nor step-by-step volume, and no Eject on the radio.
  await expect(page.locator('[data-remote-command="Eject"]')).toHaveCount(0)
  // Prev, PlayPause, Next, Stop, Mute, and the two play modes.
  await expect(page.locator('[data-remote-command]')).toHaveCount(7)
  // The radio has no finite list, so the two modes are **greyed and still
  // rendered** — not removed like Eject. Those are two different answers to
  // two different questions ("this source has no tray" versus "this source
  // has nothing to shuffle"), and only the second one was asked for here.
  await expect(page.locator('[data-remote-command="SetRandom"]')).toBeDisabled()
  await expect(page.locator('[data-remote-command="SetRepeat"]')).toBeDisabled()
  // And greyed keys that do not claim to be on: the core masks both modes on
  // a source with no finite list, so the pressed state cannot contradict the
  // greying. `files.spec.ts` proves the same thing the hard way, with the
  // setting actually armed beforehand.
  await expect(page.locator('[data-remote-command="SetRandom"]')).toHaveAttribute('aria-pressed', 'false')
  await expect(page.locator('[data-remote-command="SetRepeat"]')).toHaveAttribute('aria-pressed', 'false')
})

/**
 * The connection badge keeps its dot on a phone and drops its word.
 *
 * Only a real browser can tell: `sr-only` is a stylesheet rule, and jsdom
 * computes no style — the unit test can therefore assert the classes and the
 * presence of the text, never that the label actually takes no room here.
 *
 * `toBeHidden()` would be wrong for the same reason it is tempting: `sr-only`
 * leaves a 1×1 box, which Playwright counts as visible. The width is what
 * carries the claim.
 */
test('on a phone: the connection badge shows its dot without its label', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-connection]')).toHaveAttribute('data-connection', 'online')
  const label = page.locator('[data-connection-label]')
  // Still in the DOM, hence still announced by a screen reader: dropping the
  // word visually must not drop it from the accessibility tree.
  await expect(label).toHaveText(/^(Online|En ligne)$/)
  const box = await label.boundingBox()
  if (!box) throw new Error('the label should still occupy a (1×1) box')
  expect(box.width).toBeLessThanOrEqual(1)
})

test('on a phone: dragging the volume slider sends a SetVolume that the core echoes back', async ({ page }) => {
  // The end-to-end proof of the Slider: a real touch gesture, commands
  // sent during the drag (the last on release), and the SSE frame that comes back with the value.
  // Volume rather than progress because the radio is not `seekable`: its bar
  // is informative, without a thumb — which is verified too.
  await page.goto('/')
  await expect(page.locator('[data-volume]')).not.toHaveText('')
  await expect(page.locator('[data-bar] [role="slider"]')).toHaveCount(0)
  const slider = page.locator('[data-volume-slider]')
  const box = await slider.boundingBox()
  if (!box) throw new Error('volume slider not visible')
  const y = box.y + box.height / 2
  // The [data-volume] display is optimistic (updated as soon as the local
  // valueCommit fires), the SSE frame is not: without waiting for the POST's
  // response, the stream read further down may arrive before the core has
  // really applied the command.
  const response = page.waitForResponse(
    (r) => r.url().endsWith('/api/command') && r.request().method() === 'POST',
  )
  await page.mouse.move(box.x + box.width * 0.5, y)
  await page.mouse.down()
  await page.mouse.move(box.x + box.width * 0.25, y, { steps: 5 })
  await page.mouse.up()
  expect((await response).status()).toBe(204)
  // 20, 25 or 30 %: the exact position depends on the thumb's padding, but
  // the value is always on the grid of 5 (jsdom cannot prove it: no layout).
  await expect(page.locator('[data-volume]')).toHaveText(/^(20|25|30) %$/)
  // The 204 only proves the enqueueing: `command_post` sends on `cmd_tx` and
  // replies immediately, before the core's loop handles the command (mpv call
  // then publication on the state channel). A single SSE read may therefore
  // still land on the previous frame — hence the poll, which reopens the
  // connection until an up-to-date frame (each connection receives the
  // current state as soon as it opens, see journey.spec.ts).
  const readVolumeSse = () =>
    page.evaluate(
      () =>
        new Promise<number>((resolve, reject) => {
          const stream = new EventSource('/api/player')
          const timer = setTimeout(() => { stream.close(); reject(new Error('no frame within 2 s')) }, 2000)
          stream.onmessage = (e) => {
            clearTimeout(timer)
            stream.close()
            resolve((JSON.parse(e.data as string) as { volume: number }).volume)
          }
        }),
    )
  let lastVolume = -1
  await expect
    .poll(async () => {
      lastVolume = await readVolumeSse()
      return [20, 25, 30].includes(lastVolume)
    }, { timeout: 5000 })
    .toBe(true)
  expect([20, 25, 30]).toContain(lastVolume)
})

/**
 * No admin page may scroll sideways on a phone.
 *
 * Reported on a real phone: the update card's button row and the six-column
 * plugins table pushed the configuration page past the screen's edge, so the
 * whole page slid left and right under the finger. 360 px rather than the
 * project's Pixel 7 (412 px): it is the narrowest width still common on
 * Android, and a layout that holds there holds above it.
 *
 * The verdict is the document's own scroll width — what the finger feels —
 * and the message names the leaf elements that stick out, so a failure says
 * where to look instead of only that something is wrong. An element inside a
 * container that scrolls or clips horizontally on purpose is not a culprit:
 * it does not widen the page.
 */
for (const { path, ready } of [
  { path: '/', ready: '[data-preset-button="1"]' },
  { path: '/config', ready: '[data-plugins-table] [data-plugin-row]' },
  { path: '/system', ready: '[data-system-memory]' },
  { path: '/plugins/', ready: '[data-plugins-list] a' },
  { path: '/plugins/radio/', ready: '[data-plugin-content]' },
  { path: '/plugins/files/', ready: '[data-plugin-content]' },
  { path: '/plugins/generic-input/', ready: '[data-plugin-content]' },
]) {
  test(`on a 360 px phone: ${path} does not scroll sideways`, async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 780 })
    await page.goto(path)
    await expect(page.locator(ready).first()).toBeVisible()
    const overflow = await sidewaysCulprits(page)
    expect(overflow.scroll, `wider than the screen:\n${overflow.culprits.join('\n')}`).toBeLessThanOrEqual(
      overflow.width,
    )
  })
}

/**
 * A dialog must hold its content on a phone, at the Pixel 7's width and at 360.
 *
 * The page loop above cannot see inside an open dialog: it is fixed, so the
 * page's own scroll width never moves. The dialog's own right edge is the
 * bound here, and a row of buttons that outgrows it is a culprit.
 */
for (const viewport of [undefined /* Pixel 7, 412 px */, { width: 360, height: 780 }]) {
  for (const { path, open, dialog } of [
    { path: '/', open: '[data-sleep-timer-key]', dialog: '[data-sleep-timer-menu]' },
    { path: '/config', open: '[data-alarm-add]', dialog: '[data-alarm-dialog]' },
    { path: '/config', open: '[data-update-sources]', dialog: '[data-sources-dialog]' },
    { path: '/plugins/radio/', open: '[data-country-open]', dialog: '[role="dialog"]' },
  ]) {
    const w = viewport?.width ?? 412
    test(`on a ${w} px phone: the dialog of ${open} on ${path} holds its content`, async ({ page }) => {
      if (viewport) await page.setViewportSize(viewport)
      await page.goto(path)
      await page.locator(open).first().click()
      await expect(page.locator(dialog).first()).toBeVisible()
      const r = await sidewaysCulprits(page, dialog)
      expect(r.culprits, r.culprits.join('\n')).toEqual([])
      await page.keyboard.press('Escape')
    })
  }
}

test('on a phone: the Plugins tab leads to the list, which leads to the plugin page', async ({ page }) => {
  await page.goto('/')
  await page.locator('[data-nav-plugins]').click()
  // Three plugins with a page in the harness (radio, files, generic-input): the list.
  await expect(page).toHaveURL(/\/plugins\/$/)
  await expect(page.locator('[data-plugins-list] a')).toHaveCount(3)
  await page.locator('[data-plugins-list] a').first().click()
  await expect(page).toHaveURL(/\/plugins\/radio\/$/)
})

/**
 * The presets card on a phone: the layout toggle is always there, and a
 * vertical finger drag over the card still scrolls the page.
 *
 * The second half is the point only a real browser can make: jsdom has no
 * `touch-action` and no scrolling, so the unit tests can pin the swipe's
 * arithmetic but never that it leaves the vertical gesture to the page. The
 * gesture goes through CDP touch events (a real touch pointer, which
 * `page.mouse` is not). The harness declares one station (an earlier spec in
 * the same run may add another), so there is no second page to swipe to.
 */
test('on a phone: the presets card toggles list and pages, and a vertical drag over it scrolls the page', async ({ page }) => {
  await page.setViewportSize({ width: 412, height: 500 })
  await page.goto('/')
  const grid = page.locator('[data-preset-grid]')
  await expect(grid).toHaveAttribute('data-preset-layout', 'pages')
  const toggle = page.locator('[data-preset-layout-toggle]')
  await expect(toggle).toHaveCount(1)
  // The station count is shared server state: an earlier spec in the same run
  // may have added one. Compare the two layouts with each other rather than
  // with the fixture. With no page window (ten presets or fewer) the list
  // shows exactly the tiles the page showed.
  await expect(grid.locator('[data-preset-button]').first()).toBeVisible()
  await expect(page.locator('[data-preset-window]')).toHaveCount(0)
  const tiles = await grid.locator('[data-preset-button]').count()
  await toggle.click()
  await expect(grid).toHaveAttribute('data-preset-layout', 'list')
  await expect(page.locator('[data-preset-list] [data-preset-button]')).toHaveCount(tiles)
  await toggle.click()
  await expect(grid).toHaveAttribute('data-preset-layout', 'pages')
  await expect(page.locator('[data-preset-list]')).toHaveCount(0)

  // Bring the card in the middle of the viewport, so the finger lands on it
  // (the player card above it is taller than the viewport), while the page
  // still has room to scroll below.
  // The page height settles as the player card fills in: wait for it, or the
  // scroll position is clamped under the finger.
  await expect.poll(async () => {
    const h = await page.evaluate(() => document.documentElement.scrollHeight)
    await page.waitForTimeout(300)
    return h === (await page.evaluate(() => document.documentElement.scrollHeight))
  }).toBe(true)
  await page.evaluate(() => {
    const top = document.querySelector('[data-preset-grid]')!.getBoundingClientRect().top + window.scrollY
    window.scrollTo(0, Math.max(0, top - 250))
  })
  const box = await grid.boundingBox()
  if (!box) throw new Error('presets card not visible')
  const x = box.x + box.width / 2
  const y = box.y + Math.min(box.height / 2, 60)
  // The drag must start on the card, and the page must have room left to scroll.
  expect(await page.evaluate(([px, py]) => document.elementFromPoint(px!, py!)?.closest('[data-preset-grid]') != null, [x, y])).toBe(true)
  expect(await page.evaluate(() => document.documentElement.scrollHeight - window.innerHeight - window.scrollY)).toBeGreaterThan(0)
  const before = await page.evaluate(() => window.scrollY)
  const cdp = await page.context().newCDPSession(page)
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
  for (const step of [1, 2, 3]) {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - (200 * step) / 3 }] })
  }
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(before)
})

test('on a phone: a station takes two lines, its name then its address, each wide enough to read', async ({ page }) => {
  await page.goto('/plugins/radio/')
  const row = page.locator('[data-station-row]').first()
  const name = await row.locator('[data-station-name]').boundingBox()
  const url = await row.locator('[data-station-url]').boundingBox()
  if (!name || !url) throw new Error('station inputs not visible')
  expect(name.width).toBeGreaterThanOrEqual(180)
  expect(url.width).toBeGreaterThanOrEqual(180)
  expect(url.y).toBeGreaterThanOrEqual(name.y + name.height)
  // The move and remove buttons ride on the address's line, to its right.
  const up = await row.locator('[data-station-up]').boundingBox()
  if (!up) throw new Error('move-up button not visible')
  expect(Math.abs(up.y + up.height / 2 - (url.y + url.height / 2))).toBeLessThanOrEqual(2)
  expect(up.x).toBeGreaterThan(url.x)
})

test('at desktop width the station table keeps its header row', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 })
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-plugin-content] thead')).toBeVisible()
  const name = await page.locator('[data-station-name]').first().boundingBox()
  const url = await page.locator('[data-station-url]').first().boundingBox()
  expect(Math.abs(name!.y - url!.y)).toBeLessThan(2) // same line
})

test('on a phone: the generic-input buttons are icons, their words kept for screen readers', async ({ page }) => {
  await page.goto('/plugins/generic-input/')
  const learn = page.locator('[data-learn]').first()
  await expect(learn).toBeVisible()
  expect((await learn.boundingBox())!.width).toBeLessThanOrEqual(44)
  expect((await learn.locator('[data-label]').boundingBox())!.width).toBeLessThanOrEqual(1)
  await expect(learn).toHaveAttribute('aria-label', /.+/)
})

test('at desktop width the generic-input buttons show their words', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 })
  await page.goto('/plugins/generic-input/')
  expect((await page.locator('[data-learn] [data-label]').first().boundingBox())!.width).toBeGreaterThan(20)
  await expect(page.locator('[data-learn] [data-phone-icon]').first()).toBeHidden()
})

// --- Reordering the plugins with a finger ------------------------------------
//
// The table reflows below `md` (Pixel 7 is 412 px wide) and the ⠿ handle is
// the only place a drag can start: the rest of a row must still scroll the
// page under a finger. Every gesture goes through CDP touch events (see
// `touch.ts`). A reorder rewrites the harness's `plugins.toml`, which other
// journeys depend on: each test that may move a plugin restores the declared
// order through the API in a `finally`, as the desktop arrow journey does.

type Request = import('@playwright/test').APIRequestContext
type Page = import('@playwright/test').Page

const pluginOrder = (request: Request) => async () => {
  const status = (await (await request.get('/api/status')).json()) as { plugins: { name: string }[] }
  // One entry per name: a multi-kind plugin is listed once per kind.
  return [...new Set(status.plugins.map((p) => p.name))]
}

const pluginRow = (page: Page, name: string) =>
  page.locator('[data-plugins-table] [data-plugin-row]', { has: page.getByText(name, { exact: true }) })

const centre = (box: { x: number; y: number; width: number; height: number }) => ({
  x: box.x + box.width / 2,
  y: box.y + box.height / 2,
})

const scrollY = (page: Page) => page.evaluate(() => window.scrollY)

/** Waits for the page height to stop moving (cards fill in after load), or a
 * scroll set now is clamped later under the finger. */
async function settledHeight(page: Page) {
  await expect.poll(async () => {
    const h = await page.evaluate(() => document.documentElement.scrollHeight)
    await page.waitForTimeout(300)
    return h === (await page.evaluate(() => document.documentElement.scrollHeight))
  }).toBe(true)
}

/** Scrolls so the top of `name`'s row sits `offset` px below the viewport top. */
async function placeRow(page: Page, name: string, offset: number) {
  const box = await pluginRow(page, name).boundingBox()
  if (!box) throw new Error(`${name} row not visible`)
  await page.evaluate((y) => window.scrollTo(0, Math.max(0, y)), box.y + (await scrollY(page)) - offset)
  // Let the scroll land before measuring anything against it.
  await page.waitForTimeout(100)
}

/** True when the point is on the handle itself: a gesture that starts beside
 * it would prove nothing about the handle. */
const onHandle = (page: Page, p: { x: number; y: number }, selector: string) =>
  page.evaluate(
    ([x, y, s]) => document.elementFromPoint(x as number, y as number)?.closest(s as string) != null,
    [p.x, p.y, selector] as const,
  )

/** Puts the declared order back through the API, whatever the page did. */
async function restoreOrder(request: Request, before: string[]) {
  const order = pluginOrder(request)
  // Retried as a whole: the gesture's own move may still be in flight when
  // this starts, and land after a first pass read the order.
  await expect(async () => {
    for (const [to, name] of before.entries()) {
      const current = await order()
      if (current[to] !== name) await request.post(`/api/plugins/${name}/move`, { data: { to } })
    }
    expect(await order()).toEqual(before)
  }).toPass({ timeout: 10_000 })
}

test('on a phone: a plugin is reordered by its handle with a finger', async ({ page, request }) => {
  const order = pluginOrder(request)
  await page.goto('/config')
  const before = await order()
  const at = before.indexOf('radio')
  const next = before[at + 1]
  expect(at).toBeGreaterThanOrEqual(0)
  expect(next).toBeDefined()
  const handle = pluginRow(page, 'radio').locator('[data-plugin-drag-handle]')
  await expect(handle).toBeVisible()
  await settledHeight(page)
  const viewport = page.viewportSize()!
  await placeRow(page, 'radio', viewport.height / 2 - 60)

  try {
    const from = centre((await handle.boundingBox())!)
    expect(await onHandle(page, from, '[data-plugin-drag-handle]')).toBe(true)
    const target = (await pluginRow(page, next!).boundingBox())!
    await touchDrag(page, from, { x: from.x, y: target.y + target.height * 0.75 })
    await expect.poll(order).not.toEqual(before)
    const afterMove = await order()
    expect(afterMove.indexOf('radio')).toBe(at + 1)
    expect(afterMove.indexOf(next!)).toBe(at)

    // The table shows the server's order, unlocked, before the second drag.
    const shown = page.locator('[data-plugins-table] [data-plugin-row] [data-plugin-name]')
    await expect.poll(async () => (await shown.allTextContents()).filter((n) => before.includes(n))).toEqual(afterMove)
    await expect(handle).not.toHaveClass(/cursor-not-allowed/)
    const back = centre((await handle.boundingBox())!)
    expect(await onHandle(page, back, '[data-plugin-drag-handle]')).toBe(true)
    const above = (await pluginRow(page, next!).boundingBox())!
    await touchDrag(page, back, { x: back.x, y: above.y + above.height * 0.25 })
    await expect.poll(order).toEqual(before)
  } finally {
    await restoreOrder(request, before)
  }
})

test('on a phone: a swipe on a row scrolls; the handle does not', async ({ page, request }) => {
  const order = pluginOrder(request)
  await page.goto('/config')
  const before = await order()
  const name = before[0]!
  const row = pluginRow(page, name)
  await expect(row.locator('[data-plugin-drag-handle]')).toBeVisible()
  await settledHeight(page)
  const viewport = page.viewportSize()!
  await placeRow(page, name, viewport.height / 2)

  try {
    // A swipe on the row's name, upward: the page scrolls down under it.
    const label = centre((await row.locator('[data-plugin-name]').boundingBox())!)
    expect(await onHandle(page, label, '[data-plugin-name]')).toBe(true)
    expect(await page.evaluate(() => document.documentElement.scrollHeight - window.innerHeight - window.scrollY))
      .toBeGreaterThan(200)
    const start = await scrollY(page)
    await touchDrag(page, label, { x: label.x, y: label.y - 200 })
    await expect.poll(() => scrollY(page)).toBeGreaterThan(start + 100)
    expect(await order()).toEqual(before)

    // The same finger on the handle, 40 px down and still inside its own
    // row: a drag that goes nowhere, and the page stays where it is. Downward
    // would scroll the page up, so there must be room above for that to show.
    await page.waitForTimeout(300) // the swipe's momentum, if any, ends
    const handle = centre((await row.locator('[data-plugin-drag-handle]').boundingBox())!)
    expect(await onHandle(page, handle, '[data-plugin-drag-handle]')).toBe(true)
    const own = (await row.boundingBox())!
    const end = { x: handle.x, y: handle.y + 40 }
    expect(end.y).toBeLessThan(own.y + own.height)
    const still = await scrollY(page)
    expect(still).toBeGreaterThan(40)
    await touchDrag(page, handle, end)
    await page.waitForTimeout(300)
    expect(Math.abs((await scrollY(page)) - still)).toBeLessThanOrEqual(2)
    expect(await order()).toEqual(before)
  } finally {
    await restoreOrder(request, before)
  }
})

test('on a phone: holding a dragged row near the edge scrolls the page', async ({ page, request }) => {
  const order = pluginOrder(request)
  await page.goto('/config')
  const before = await order()
  const handle = pluginRow(page, 'radio').locator('[data-plugin-drag-handle]')
  await expect(handle).toBeVisible()
  await settledHeight(page)
  const viewport = page.viewportSize()!

  // Room to scroll in one direction, measured, and the edge to hold at: below
  // with the row high in the viewport if the page goes on far enough under
  // it, above with the row low in the viewport otherwise.
  await placeRow(page, 'radio', 150)
  const roomBelow = await page.evaluate(
    () => document.documentElement.scrollHeight - window.innerHeight - window.scrollY,
  )
  const down = roomBelow >= 400
  if (!down) await placeRow(page, 'radio', viewport.height - 250)
  const room = down ? roomBelow : await scrollY(page)
  // A page too short to scroll 400 px fails here, loudly, rather than letting
  // the assertion below pass on a scroll it never had room to make.
  expect(room).toBeGreaterThanOrEqual(400)

  try {
    const from = centre((await handle.boundingBox())!)
    expect(await onHandle(page, from, '[data-plugin-drag-handle]')).toBe(true)
    // 90 px from the edge: inside the 120 px the list scrolls from, outside
    // Sortable's default 30 px — so this fails if that tuning is lost.
    const to = { x: from.x, y: down ? viewport.height - 90 : 90 }
    const start = await scrollY(page)
    await touchDrag(page, from, to, { holdMs: 600 })
    const moved = (await scrollY(page)) - start
    // The drop reorders too (the row went past others): let its answer settle
    // so the restore below does not race it.
    await expect(handle).not.toHaveClass(/cursor-not-allowed/)
    expect(down ? moved : -moved).toBeGreaterThanOrEqual(300)
  } finally {
    await restoreOrder(request, before)
  }
})
