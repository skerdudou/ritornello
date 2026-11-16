import { expect, test } from '@playwright/test'

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
    const overflow = await page.evaluate(() => {
      const root = document.documentElement
      const width = root.clientWidth
      const culprits: string[] = []
      for (const el of Array.from(document.body.querySelectorAll<HTMLElement>('*'))) {
        if (el.children.length > 0) continue
        const box = el.getBoundingClientRect()
        if (box.width === 0 || box.right <= width + 0.5) continue
        // A fixed element (the toaster's empty list) does not widen the page
        // either, whatever its box says.
        let clipped = false
        for (let p: HTMLElement | null = el; p && p !== document.body; p = p.parentElement) {
          const style = getComputedStyle(p)
          if (style.position === 'fixed' || (p !== el && style.overflowX !== 'visible')) { clipped = true; break }
        }
        if (clipped) continue
        const marker = Array.from(el.attributes).find((a) => a.name.startsWith('data-'))?.name
        const text = (el.textContent ?? '').trim().slice(0, 40)
        culprits.push(`${el.tagName.toLowerCase()}${marker ? `[${marker}]` : ''} "${text}" right=${Math.round(box.right)}`)
      }
      return { scroll: root.scrollWidth, width, culprits }
    })
    expect(overflow.scroll, `wider than the screen:\n${overflow.culprits.join('\n')}`).toBeLessThanOrEqual(
      overflow.width,
    )
  })
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
 * `page.mouse` is not). The harness declares one station, so there is no
 * second page to swipe to.
 */
test('on a phone: the presets card toggles list and pages, and a vertical drag over it scrolls the page', async ({ page }) => {
  await page.setViewportSize({ width: 412, height: 500 })
  await page.goto('/')
  const grid = page.locator('[data-preset-grid]')
  await expect(grid).toHaveAttribute('data-preset-layout', 'pages')
  const toggle = page.locator('[data-preset-layout-toggle]')
  await expect(toggle).toHaveCount(1)
  await toggle.click()
  await expect(grid).toHaveAttribute('data-preset-layout', 'list')
  await expect(page.locator('[data-preset-list] [data-preset-button]')).toHaveCount(1)
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
