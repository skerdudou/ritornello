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
  await expect(page.locator('[data-remote-command="SetRepeatAll"]')).toBeDisabled()
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
  // The end-to-end proof of the Slider: a real touch gesture, a single
  // command on release, and the SSE frame that comes back with the value.
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
  // Between 20 and 30 %: the exact position depends on the thumb's padding.
  await expect(page.locator('[data-volume]')).toHaveText(/^(2\d|30) %$/)
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
      return lastVolume >= 20 && lastVolume <= 30
    }, { timeout: 5000 })
    .toBe(true)
  expect(lastVolume).toBeGreaterThanOrEqual(20)
  expect(lastVolume).toBeLessThanOrEqual(30)
})

test('on a phone: the Plugins tab leads to the list, which leads to the plugin page', async ({ page }) => {
  await page.goto('/')
  await page.locator('[data-nav-plugins]').click()
  // Three plugins with a page in the harness (radio, files, generic-input): the list.
  await expect(page).toHaveURL(/\/plugins\/$/)
  await expect(page.locator('[data-plugins-list] a')).toHaveCount(3)
  await page.locator('[data-plugins-list] a').first().click()
  await expect(page).toHaveURL(/\/plugins\/radio\/$/)
})
