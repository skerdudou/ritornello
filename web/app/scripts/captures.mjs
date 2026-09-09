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
// `scrollTo`: a selector to bring into view before the shot. The config page
// is long, and its most interesting card — updates and the plugins table —
// sits below the fold; a screenshot of its top would show the audio picker
// instead of the thing the README is pointing at.
async function capture(browser, name, { width, height, mode, path = '/', wait = 800, scrollTo }) {
  const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 2 })
  try {
    await page.goto(`${BASE}/`)
    await page.waitForSelector('[data-preset-button]')
    // The mode is a device setting (PUT /api/theme), not a browser one.
    const theme = await page.evaluate(() => fetch('/api/theme').then((r) => r.json()))
    try {
      await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), { ...theme, mode })
      await page.goto(`${BASE}${path}`)
      if (scrollTo) {
        // `scrollIntoViewIfNeeded` and not a hash in the URL: the router owns
        // the scroll position, and a hash it did not put there is not honoured
        // on a fresh load — the shot would silently be of the page's top.
        await page.locator(scrollTo).scrollIntoViewIfNeeded()
      }
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

const SHOTS = {
  'home-light': { width: 1280, height: 800, mode: 'light' },
  'home-dark': { width: 1280, height: 800, mode: 'dark' },
  'home-phone': { width: 390, height: 844, mode: 'light' },
  'radio-admin': { width: 1280, height: 800, mode: 'light', path: '/plugins/radio/' },
  // Ninety seconds, not twenty-five. Measured: the history is a sliding
  // window fed by the page's own polling, so at 25 s it still reads "0 min
  // window" over an empty frame — a graph the README's caption promises and
  // the picture does not show. At 90 s it reads "1 min window" and has a
  // curve. The CPU usage, a delta, is already right well before that.
  system: { width: 1280, height: 800, mode: 'light', path: '/system', wait: 90_000 },
  // The flagship of the update work: the check card, the automatic policy
  // with its prerelease switch, and the plugins table underneath.
  'config-update': { width: 1280, height: 800, mode: 'light', path: '/config', scrollTo: '#update' },
}

// Named on the command line, one or more, to redo a single shot without
// disturbing the others: the home shots depend on what the station happens to
// be playing, and re-running everything to fix one of them can lose a good
// one. No argument means all of them.
const wanted = process.argv.slice(2)
const unknown = wanted.filter((n) => !(n in SHOTS))
if (unknown.length > 0) {
  throw new Error(`unknown capture(s): ${unknown.join(', ')} — known: ${Object.keys(SHOTS).join(', ')}`)
}
const todo = wanted.length > 0 ? wanted : Object.keys(SHOTS)

const browser = await chromium.launch()
try {
  for (const name of todo) {
    await capture(browser, name, SHOTS[name])
  }
} finally {
  // Otherwise a Chromium browser stays open (and the process never exits) as
  // soon as one of the four shots fails.
  await browser.close()
}
console.log(`screenshots written to ${OUT}`)
