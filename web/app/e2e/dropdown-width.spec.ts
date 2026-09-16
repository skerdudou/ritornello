import { expect, test } from '@playwright/test'

/**
 * Opening a dropdown must not move the page sideways.
 *
 * reka-ui locks the body while a Select is open, and compensates for the
 * scrollbar it believes is about to disappear by adding a `padding-right` of
 * `innerWidth - documentElement.clientWidth` to the body. That reasoning is
 * sound on a page whose scrollbar really does take its space back — and wrong
 * here, because `app.css` sets `html { scrollbar-gutter: stable }`: the gutter
 * stays reserved whatever the body's overflow, so the compensation is pure
 * loss and the centered container slides half of it to the left (measured:
 * 7.5px of a 15px gutter, on a 1280px viewport).
 *
 * The two settings are therefore a pair: whoever reserves the gutter in CSS
 * must also tell reka-ui to stop compensating (`scroll-body="false"` on the
 * shell's ConfigProvider, see App.vue). Changing one without the other brings
 * the jump back, in one direction or the other. `journey.spec.ts` pins the CSS
 * half of that pair; this file measures what the two produce together.
 *
 * ## Why this file has a project of its own
 *
 * It runs under the `scrollbars` project, which drops Playwright's default
 * `--hide-scrollbars`: with that argument the page has zero-width scrollbars,
 * `innerWidth - clientWidth` is 0, reka-ui compensates nothing, and the defect
 * is *invisible* — measured on both sides of the fix. See playwright.config.ts.
 */

/**
 * Horizontal geometry of the two centered columns a dropdown has no business
 * moving: the header's nav and the content's `main`.
 *
 * Horizontal only — clicking a trigger may scroll it into view, and vertical
 * movement is not what is under test.
 *
 * Deliberately *not* `documentElement.clientWidth`: measured, it does grow by
 * the gutter's width while the body is locked (1265 -> 1280 -> 1265 on a
 * 1280px viewport), because nothing overflows any more and the viewport stops
 * reporting a scrollbar. Nothing follows it — `body.clientWidth` stays put,
 * and so does the box fixed elements are laid out against (measured on a
 * full-width `position: fixed` probe: 1265 throughout, so BottomNav and the
 * toasts do not move either). Asserting on it would pin an invisible detail
 * and fail on a correct page.
 */
async function columns(page: import('@playwright/test').Page) {
  return page.evaluate(() => {
    const box = (selector: string) => {
      const el = document.querySelector(selector)
      if (!el) throw new Error(`${selector} is missing from the page`)
      const { x, width } = el.getBoundingClientRect()
      return { x, width }
    }
    return { nav: box('header nav'), main: box('main') }
  })
}

test('opening a dropdown leaves the page width and position alone', async ({ page }) => {
  await page.goto('/config')

  const trigger = page.locator('[data-update-policy]')
  await expect(trigger).toBeVisible()

  const room = await page.evaluate(() => ({
    // > 0 only when the browser draws a scrollbar that takes layout space.
    // With the stable gutter this also implies the page scrolls, so it is the
    // precondition of the whole measurement.
    deficit: window.innerWidth - document.documentElement.clientWidth,
    scrollable: document.documentElement.scrollHeight > document.documentElement.clientHeight,
  }))

  // A configuration page that no longer scrolls would make the case
  // unreachable rather than fixed: that is a failure, not a skip.
  expect(room.scrollable, 'the configuration page must still scroll').toBe(true)
  test.skip(
    room.deficit === 0,
    'this browser overlays its scrollbars: there is no gutter to mis-compensate',
  )

  const before = await columns(page)

  await trigger.click()
  // The lock lands with the popup, so measuring before it is visible would
  // read the page in its unlocked state and pass for the wrong reason.
  await expect(page.getByRole('listbox')).toBeVisible()

  expect(await columns(page), 'the page moved sideways while the list was open').toEqual(before)
  // The cause, pinned next to its effect: this is the single declaration the
  // fix controls, and the one a future reka-ui upgrade could start writing
  // again through some other route.
  expect(await page.evaluate(() => getComputedStyle(document.body).paddingRight)).toBe('0px')

  // And it comes back unchanged once the dropdown closes, so that a fix which
  // merely moved the jump to the closing edge would not pass either.
  await page.keyboard.press('Escape')
  await expect(page.getByRole('listbox')).toHaveCount(0)
  expect(await columns(page), 'the page moved sideways when the list closed').toEqual(before)
})
