// Regenerates docs/captures/*.png from a running core (see docs/development.md).
// Hand-made screenshots went stale with every piece of work; these are redone
// in one command, in both modes and at both widths.
import { chromium } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { resolve } from 'node:path'

const BASE = process.env.RITORNELLO_URL ?? 'http://127.0.0.1:8099'

// `../../docs/captures` only makes sense when launched from `web/app`: anywhere
// else it would silently resolve to another folder (possibly outside the
// repository) without ever touching the screenshots actually documented. Better
// to fail loudly than to write to the wrong place without a word.
const cwd = process.cwd().replace(/\\/g, '/')
if (!cwd.endsWith('/web/app')) {
  throw new Error(`run this script from web/app (current cwd: ${process.cwd()})`)
}
const OUT = resolve(process.cwd(), '../../docs/captures')
mkdirSync(OUT, { recursive: true })

// `wait`: delay before the shot, in ms. 800 is enough everywhere except on
// /system, whose CPU usage is a delta computed in the page and whose history is
// a sliding window: opened less than one refresh cycle ago, they show "—" and
// an empty curve.
async function capture(browser, name, { width, height, mode, path = '/', wait = 800 }) {
  const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 2 })
  try {
    await page.goto(`${BASE}/`)
    await page.waitForSelector('[data-preset-button]')
    // The mode is a device setting (PUT /api/theme), not a browser one.
    const theme = await page.evaluate(() => fetch('/api/theme').then((r) => r.json()))
    try {
      await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), { ...theme, mode })
      await page.goto(`${BASE}${path}`)
      await page.waitForTimeout(wait)
      await page.screenshot({ path: resolve(OUT, `${name}.png`), fullPage: false })
    } finally {
      // Restored to the state we found even if the shot crashes midway: without
      // this `finally`, a failure in the middle of the script would leave the
      // real device in the mode of the last attempted shot.
      await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), theme)
    }
  } finally {
    await page.close()
  }
}

const browser = await chromium.launch()
try {
  await capture(browser, 'home-light', { width: 1280, height: 800, mode: 'light' })
  await capture(browser, 'home-dark', { width: 1280, height: 800, mode: 'dark' })
  await capture(browser, 'home-phone', { width: 390, height: 844, mode: 'light' })
  await capture(browser, 'radio-admin', { width: 1280, height: 800, mode: 'light', path: '/plugins/radio/' })
  await capture(browser, 'system', { width: 1280, height: 800, mode: 'light', path: '/system', wait: 25_000 })
} finally {
  // Otherwise a Chromium browser stays open (and the process never exits) as
  // soon as one of the four shots fails.
  await browser.close()
}
console.log(`screenshots written to ${OUT}`)
