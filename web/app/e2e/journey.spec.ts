import { expect, test } from '@playwright/test'

// Reads a **computed** CSS variable on the document root: it is the only
// proof that a theme engine really acts on the rendering — an attribute or a
// class only proves the intent, not the effect.
const cssVariable = (page: import('@playwright/test').Page, name: string) =>
  page.evaluate(
    (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(),
    name,
  )

test('navigation between the home page, the config and the plugin pages', async ({ page, request }) => {
  await page.goto('/')
  await expect(page.locator('[data-preset-button="1"]')).toBeVisible()
  // The harness declares a single station (stations.toml): the grid only
  // shows that real number, and no pagination arrows since the count does
  // not exceed nine.
  await expect(page.locator('[data-preset-button]')).toHaveCount(1)
  await expect(page.locator('[data-preset-prev]')).toHaveCount(0)
  await expect(page.locator('[data-preset-next]')).toHaveCount(0)
  await page.goto('/config')
  // `getByText('radio')` alone is ambiguous: the header also lists the admin
  // plugins by name (see App.vue), so "radio" shows up there in addition to
  // the status table cell — hence targeting by role. And `exact`: the switch
  // cell is named "Enable or disable radio", which the partial name also
  // caught.
  await expect(page.getByRole('cell', { name: 'radio', exact: true })).toBeVisible()

  // The update card, and its two buttons. Counted rather than merely found:
  // a button removed by a refactor must turn this red.
  await expect(page.locator('[data-update-card]')).toBeVisible()
  await expect(page.locator('[data-update-card] button')).toHaveCount(2)
  // Against the real `/api/update` this harness serves, not only the page:
  // `not.toHaveText('')` and a `toHaveText` pinned to the client's own
  // pre-fetch default would both stay green against a broken or 404
  // endpoint — the SPA renders that default either way. The harness has no
  // access to GitHub and never presses "Check" in this journey, so the
  // server's own answer is deterministically `never_checked`.
  const updateState = await (await request.get('/api/update')).json()
  expect(updateState.outcome).toEqual({ kind: 'never_checked' })
  await expect(page.locator('[data-update-summary]')).toHaveText('Never checked')
  // The policy selector, with its hour, its cadence, and the button that
  // saves them — every added control on this card, not only the two that
  // happened to be listed first.
  await expect(page.locator('[data-update-policy]')).toBeVisible()
  await expect(page.locator('[data-update-hour]')).toBeVisible()
  await expect(page.locator('[data-update-cadence]')).toBeVisible()
  // The prerelease switch, and **the position it starts in** against a real
  // core with a real state file: off. That default is the whole safety of the
  // feature — a device nobody told otherwise is never offered unfinished
  // software — and it is the one thing a unit test with a hand-written
  // payload cannot show, since it supplies the value it then asserts.
  await expect(page.locator('[data-update-prereleases]')).toHaveAttribute(
    'aria-checked',
    'false',
  )
  await expect(page.locator('[data-update-policy-change]')).toHaveCount(1)

  // The plugins table's columns, against a real core. Nothing here counted
  // them before, so the Version column could have been added — or dropped
  // again — without a single test noticing, while the design assumed this
  // journey was the barrier that kept them in sync. Eight headers, exact —
  // the six pre-existing ones plus the order arrows and the install/uninstall
  // gestures (task 18) — is what makes it a lock rather than a lower bound.
  const pluginsTable = page.locator('[data-plugins-table]')
  await expect(pluginsTable.locator('thead th')).toHaveCount(8)
  // And the cell under it carries a real version. Asserted as "not the em
  // dash placeholder" rather than as a fixed string: the SDK derives the
  // number from the plugin crate's own `CARGO_PKG_VERSION`, so a version is
  // certainly there, but pinning it here would mean editing this file at
  // every bump — a test that fails for being right.
  const radioVersion = pluginsTable
    .locator('[data-plugin-row]', { has: page.getByText('radio', { exact: true }) })
    .locator('[data-plugin-version]')
  await expect(radioVersion).toBeVisible()
  await expect(radioVersion).not.toHaveText('—')
  // The order arrows exist and are greyed out at either end of the declared
  // list — counted, not pressed: a reorder here writes the harness's own
  // `plugins.toml` and changes the source cycle three other journeys depend
  // on (see the dedicated reorder journey below, which presses one and puts
  // it back).
  await expect(pluginsTable.locator('[data-plugin-row]').first().locator('[data-plugin-up]')).toBeDisabled()
  await expect(pluginsTable.locator('[data-plugin-row]').last().locator('[data-plugin-down]')).toBeDisabled()

  // A save announces its outcome with a notification. Reported in use: the
  // vue-sonner stylesheet was imported nowhere, so the message rendered
  // **in the document flow** -- a bare text at the bottom of the page, which
  // had to be scrolled to. Nothing but this journey can catch it: jsdom
  // computes no style, and the equivalent unit assertion would wrongly pass.
  await page.locator('[data-seek-change]').click()
  const notif = page.locator('[data-sonner-toast]').first()
  await expect(notif).toBeVisible()
  // `fixed`: this is the proof that the stylesheet is loaded. Without it, the
  // container stays `static` and the message falls into the document flow.
  const container = page.locator('[data-sonner-toaster]')
  await expect(container).toHaveCSS('position', 'fixed')
  // Centered at the top and colored by type, as requested: these two
  // attributes are what vue-sonner sets from the `<Toaster />` props, and
  // nothing else in the UI carries them.
  await expect(container).toHaveAttribute('data-y-position', 'bottom')
  await expect(container).toHaveAttribute('data-x-position', 'center')
  await expect(container).toHaveAttribute('data-rich-colors', 'true')
  // Plugin page: the ESM module is loaded dynamically and resolved through the
  // import map — that is what no unit test can verify.
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  await page.goto('/plugins/generic-input/')
  // Twenty-five fixed rows — the two seek keys, then the two play modes —
  // plus one per source the core announces, here `radio` and `files`.
  //
  // This count is the only lock that counts the **rendered** rows: the unit
  // tests lock the `ACTIONS` list upstream, but none of them mounts the real
  // page, and none of them fetches the source catalogue over HTTP. Keep it
  // exact rather than a lower bound — it is what caught the source rows and
  // the play-mode toggles landing while it still said twenty-three.
  await expect(page.locator('[data-action-row]')).toHaveCount(27)
  // And they are the last rows, in the catalogue's order, which is the order
  // the source key walks. A count alone would not notice a row rendering
  // empty — precisely what an unreachable catalogue would produce.
  //
  // **`radio` then `files`, which is the order of the fixture's
  // `plugins.toml` and no longer the alphabet.** This assertion read
  // `files` first until the auto-update branch landed, and it was right to
  // fail: that branch deliberately made the source cycle follow the
  // declared order, and this is the only test in the repository that
  // observes the rendered consequence. It could not be updated with the
  // change because the e2e suite cannot run on the development machine.
  const labels = page.locator('[data-action-row] td:first-child')
  await expect(labels.nth(25)).toHaveText(/radio/)
  await expect(labels.nth(26)).toHaveText(/files/)
})

test('a single Vue instance serves the shell and the plugin modules', async ({ page }) => {
  const requests: string[] = []
  page.on('request', (r) => requests.push(new URL(r.url()).pathname))
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  // The central property of the architecture: the shell and the plugin module
  // both import 'vue' through the import map, which resolves to the same
  // stable URL — a single request, hence a single loaded instance.
  expect(requests.filter((p) => p === '/assets/vue.js')).toHaveLength(1)
  expect(requests).toContain('/plugins/radio/ui.js')
  expect(requests).toContain('/plugins/radio/ui.css')
})

test('the player state arrives as a pushed stream as soon as the connection opens', async ({ page }) => {
  await page.goto('/')
  // Only end-to-end proof of the SSE route: a real browser `EventSource`,
  // served by the real Rust binary. No unit test covers the complete chain
  // (axum -> watch channel -> EventSource), and the property verified here is
  // precisely the one that avoids an empty tab: the current state is emitted
  // **as soon as the connection opens**, without waiting for a change.
  const first = await page.evaluate(
    () =>
      new Promise<string>((resolve, reject) => {
        const stream = new EventSource('/api/player')
        const timer = setTimeout(() => {
          stream.close()
          reject(new Error('no frame within 5 s'))
        }, 5000)
        stream.onmessage = (e) => {
          clearTimeout(timer)
          stream.close()
          resolve(e.data as string)
        }
      }),
  )
  // The harness declares no `metadata` plugin and reads no real stream: the
  // state is therefore empty of metadata, but it already carries source and
  // volume — that is what lets the UI show the player state without polling.
  const state = JSON.parse(first) as {
    source: string
    volume: number
    muted: boolean
    standby: boolean
    preset: number | null
    title: string | null
  }
  expect(state.source).toBe('radio')
  expect(state.volume).toBeGreaterThan(0)
  expect(state.muted).toBe(false)
  expect(state.standby).toBe(false)
  // Waking the radio plays preset 1 (the harness's stations.toml) and declares
  // it: this is the end-to-end proof that the active key travels from the
  // plugin to the SPA. FIP emits no ICY, hence no title.
  expect(state.preset).toBe(1)
  expect(state.title).toBeNull()
  // And the player panel shows them.
  await expect(page.locator('[data-source]')).toHaveText('radio')
  await expect(page.locator('[data-volume]')).toHaveText(`${state.volume} %`)
  await expect(page.locator('[data-now-playing]')).toHaveCount(0)
  // The key of the playing preset is highlighted, and only that one.
  await expect(page.locator('[data-preset-button="1"]')).toHaveAttribute('data-preset-active', 'true')
  await expect(page.locator('[data-preset-active]')).toHaveCount(1)
})

test('light/dark toggle, applied and persisted', async ({ page }) => {
  await page.goto('/')
  const light = await cssVariable(page, '--background')
  await page.getByLabel('toggle theme mode').click()
  await expect.poll(() => cssVariable(page, '--background')).not.toBe(light)
  const dark = await cssVariable(page, '--background')
  // Server-side persistence: a reload must keep the mode.
  await page.reload()
  await expect.poll(() => cssVariable(page, '--background')).toBe(dark)
  expect(await page.evaluate(() => document.documentElement.classList.contains('dark'))).toBe(true)
})

test('picking a theme in the popover, applied and persisted', async ({ page }) => {
  await page.goto('/')
  await page.getByLabel('pick theme').click()
  await page.locator('[data-preset="vercel"]').click()
  const primary = await cssVariable(page, '--primary')
  await page.reload()
  await expect.poll(() => cssVariable(page, '--primary')).toBe(primary)
  await page.getByLabel('pick theme').click()
  await expect(page.locator('[data-preset="vercel"]')).toHaveAttribute('data-active', 'true')
})

test('the popover lists the 42 themes and filters them', async ({ page }) => {
  await page.goto('/')
  await page.getByLabel('pick theme').click()
  await expect(page.locator('[data-preset]')).toHaveCount(42)
  // Real typing rather than a `setInputFiles`/direct evaluation: the `v-model`
  // of the kit's `Input` component goes through `useVModel({passive: true})`
  // from @vueuse/core, a path no unit test covers.
  await page.getByPlaceholder('filter').fill('northern')
  await expect(page.locator('[data-preset]')).toHaveCount(1)
})

test('adding and saving a station, read back from the API', async ({ page, request }) => {
  await page.goto('/plugins/radio/')
  await page.locator('[data-add]').click()
  const rows = page.locator('[data-station-name]')
  await rows.last().fill('Test E2E')
  await page.locator('[data-station-url]').last().fill('http://exemple.test/flux.mp3')
  await page.locator('[data-save]').click()
  const data = await (await request.get('/plugins/radio/api/data')).json()
  expect(data.stations.map((s: { name: string }) => s.name)).toContain('Test E2E')
  // Numbering by position: the initial station (the harness's stations.toml)
  // occupies preset 1, so the added station takes 2.
  expect(data.stations.find((s: { name: string }) => s.name === 'Test E2E').preset).toBe(2)
})

test('key learning: the view reaches a defined state', async ({ page }) => {
  await page.goto('/plugins/generic-input/')
  const first = page.locator('[data-action-row]').first()
  await first.locator('[data-learn]').click()
  // Two outcomes are legitimate depending on whether the environment exposes
  // a readable evdev device or not, and both are defined states:
  //  - no device          -> "No input device detected"
  //  - learning started   -> "Press a key on the device…"
  // We assert on this closed set of messages (values from
  // crates/ritornello-plugin-generic-input/src/locales/en.toml — the embedded
  // English, since RITORNELLO_LOCALES is not set by the harness), and not on
  // "some text": a test that accepts anything proves nothing.
  await expect(
    page.getByText(/No input device detected|Press a key on the device/),
  ).toBeVisible()
})

// The System tab: rendering and navigation only. NO power action is confirmed
// here — the harness runs a real core on the development machine, where
// confirming "Shut down" would halt it and "Restart Ritornello" would kill the
// harness mid-run. The dialog and the sending are covered by the vitest tests,
// which have no machine to lose.
test('System tab: metrics and buttons present', async ({ page }) => {
  await page.goto('/system')
  // Two assertions and not one: `not.toHaveText` alone would also pass on a
  // blank page — an absent locator cannot equal "—". The first requires the
  // field to exist, the second that it carries a real value.
  await expect(page.locator('[data-system-kernel]')).toBeVisible()
  await expect(page.locator('[data-system-kernel]')).not.toHaveText('—')
  await expect(page.locator('[data-system-memory]')).toBeVisible()
  await expect(page.locator('[data-system-disk]')).toBeVisible()
  await expect(page.locator('[data-power-poweroff]')).toBeVisible()
  await expect(page.locator('[data-power-restart]')).toBeVisible()
  // The latest errors live here, no longer on the Configuration page: this is
  // the page one opens when the device misbehaves.
  await expect(page.locator('[data-logs-card]')).toBeVisible()
  await page.goto('/config')
  await expect(page.locator('[data-logs-card]')).toHaveCount(0)
  // The navigation link exists from the home page. Scoped to the top nav
  // (visible on this desktop viewport): since task 11, a second nav — the
  // phone's bottom bar — carries the same link, hidden but still in the DOM,
  // which would make the bare selector ambiguous.
  await page.goto('/')
  await expect(page.locator('[data-top-nav] a[href="/system"]')).toBeVisible()
})

/**
 * The connection badge, against a real core.
 *
 * Its whole chain only exists here: the probe of `/api/system`, the state
 * `useMetrics` derives from the answer, and the header that renders it. The
 * unit tests stub the probe, so they prove the mapping and never that a
 * running server turns the badge green.
 */
test('the header reports the core as reachable, on every page', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-connection]')).toHaveAttribute('data-connection', 'online')
  // A regex and not a fixed string: what must be caught here is a raw
  // translation key reaching the screen (`connection_online`), not the
  // language the disposable core happens to start in.
  await expect(page.locator('[data-connection-label]')).toHaveText(/^(Online|En ligne)$/)
  // It belongs to the shell, so it survives navigation — including onto a
  // plugin page, where nothing else would report a device that went away.
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-connection]')).toHaveAttribute('data-connection', 'online')
})
/**
 * **A plugin's CSS must not undo the shell's.**
 *
 * Real regression: the two Tailwind passes wrote into the same `utilities`
 * layer, and the plugin's stylesheet — injected afterwards, and deliberately
 * left in place — won at equal specificity. The `class="hidden"` of the
 * `InputAdmin` file field thus overrode the navigation bar's `md:flex`, which
 * disappeared for the rest of the session.
 *
 * A journey and not a unit test: the defect lives neither in the markup nor in
 * the components but in the CSS cascade of two actually served stylesheets,
 * which jsdom does not compute.
 */
test('the top menu survives a visit to generic-input', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-top-nav]')).toBeVisible()

  // SPA navigation (a click), and not a `goto`: this is the real case. A full
  // reload would throw away the injected stylesheet, and so would precisely
  // mask the defect.
  await page.locator('[data-top-nav] a', { hasText: 'generic-input' }).click()
  await expect(page.locator('[data-device-select]')).toBeVisible()
  await expect(page.locator('[data-top-nav]')).toBeVisible()

  // And the plugin's file field — the one whose `.hidden` overrode the
  // shell — stays hidden: the `plugin` layer must not have disarmed the
  // plugin's CSS on its own markup.
  await expect(page.locator('[data-import]')).toBeHidden()

  await page.locator('[data-top-nav] a', { hasText: 'Configuration' }).click()
  await expect(page.locator('[data-top-nav]')).toBeVisible()
})

test('a file the app does not have gets a 404, not the whole page', async ({ page, request }) => {
  // Reported in use: an icon request that never settled, on a project that
  // had no icon. No `rel="icon"` was declared, so every browser asked for
  // `/favicon.ico` by itself — and the SPA fallback answered with the entire
  // HTML shell and a 200. A browser cannot decode that as an image and has
  // nothing worth keeping from it, so it asked again on every load, for ever.
  //
  // Two halves, and both are needed. The 404 stops the shell from pretending
  // to be a file; the declared icon is what stops the request being made at
  // all.
  const missing = await request.get('/favicon.ico')
  expect(missing.status()).toBe(404)

  await page.goto('/')
  const icon = page.locator('link[rel="icon"]')
  await expect(icon).toHaveCount(1)
  // Inlined, so the icon costs no request of its own.
  expect(await icon.getAttribute('href')).toMatch(/^data:image\/png;base64,/)
})

test('the usable width does not move when content starts or stops scrolling', async ({ page }) => {
  // Reported in use: moving from page to page, the layout jumps sideways. A
  // page short enough to need no scrollbar gives the document the classic
  // scrollbar's width back, and the next page takes it away again — every
  // element shifts horizontally, twice per navigation. The deferred
  // placeholders made it systematic, since a page now starts out empty.
  //
  // The remedy must not be a permanently visible scrollbar: the gutter is
  // reserved, the bar itself is drawn only when it can be used.
  //
  // **Measured, not assumed**: the browser these journeys run in draws overlay
  // scrollbars — `window.innerWidth` equals `documentElement.clientWidth`, so
  // its scrollbar takes zero width. Emptying and refilling the page therefore
  // moves nothing here, and a width comparison would pass with or without the
  // fix. It would be a test that proves nothing.
  //
  // What is checked instead is the rule reaching the served stylesheet and
  // being understood by a real engine — the thing that can silently break in
  // this project (a rule that never reaches the sheet has bitten before) and
  // that jsdom, computing no style at all, can never see.
  await page.goto('/config')
  await expect(page.locator('html')).toHaveCSS('scrollbar-gutter', 'stable')
})

test('the stable bundles are served immutable, under a single URL each', async ({ page, request }) => {
  // Two invariants in one journey, because they are two faces of the same
  // decision. The fingerprint is what lets a **stable** name be cached for
  // good — that name is the plugin UI contract and cannot carry a hash. And
  // it must appear in the import map ONLY: a module is identified by its
  // resolved URL, so a second reachable URL for `vue.js` would evaluate Vue
  // twice and split the reactivity graph between shell and plugins.
  const requests: string[] = []
  page.on('request', (r) => requests.push(new URL(r.url()).pathname))
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()

  // One request for the Vue bundle, hence one instance.
  expect(requests.filter((p) => p === '/assets/vue.js')).toHaveLength(1)

  // The import map carries a fingerprint for both stable names.
  const html = await (await request.get('/')).text()
  const map = html.match(/<script type="importmap">([\s\S]*?)<\/script>/)?.[1] ?? ''
  expect(map).toMatch(/"vue":"\/assets\/vue\.js\?v=[0-9a-f]+"/)
  expect(map).toMatch(/"@ritornello\/ui":"\/assets\/ui-kit\.js\?v=[0-9a-f]+"/)

  // And that URL is served without any need to revalidate it.
  const stamped = map.match(/"vue":"([^"]+)"/)![1]!
  const head = await request.get(stamped)
  expect(head.headers()['cache-control']).toContain('immutable')
})

/**
 * Ruling 77 sends this here: task 16 built `POST /api/plugins/{name}/move`
 * and its arrows without a single web file to carry its own e2e debt, and
 * "the columns are there and the ends are greyed" (asserted above, in the
 * first journey) is exactly the shape that stays green against an endpoint
 * that writes nothing — a client-side default satisfies it regardless. So
 * this journey presses an arrow for real and reads the order back from the
 * server, not from the page's own idea of what it just sent.
 *
 * It is also the one arrow-press this suite is on record refusing (Step 4):
 * a reorder rewrites the harness's `plugins.toml` and changes the source
 * cycle three other journeys depend on — one cause already produced four red
 * tests here once. The reconciliation is to press it and immediately press
 * it back, checked against `/api/status` both times, with a `finally` that
 * restores the order through the API directly if anything above it throws —
 * so no other journey, before or after this one, in this file or in
 * `files.spec.ts`, observes anything but the declared order the harness
 * started with **on the happy path, and on a failing one alike** (review of
 * this task, I2: the `finally` is what makes that true on both).
 */
test('an order arrow writes a real reorder, checked against the server, and is put back', async ({
  page,
  request,
}) => {
  const order = async () => {
    const status = (await (await request.get('/api/status')).json()) as {
      plugins: { name: string }[]
    }
    // One entry per **name**, in first-seen (i.e. file) order: a multi-kind
    // plugin otherwise counts more than once and could look like a move that
    // never happened.
    return [...new Set(status.plugins.map((p) => p.name))]
  }

  await page.goto('/config')
  const before = await order()
  expect(before.length).toBeGreaterThanOrEqual(2)

  const pluginsTable = page.locator('[data-plugins-table]')
  const radioRow = pluginsTable.locator('[data-plugin-row]', {
    has: page.getByText('radio', { exact: true }),
  })

  // I2 (fix round 1): without this `finally`, a failure between the two
  // presses below — the second click's actionability timeout, a hiccup
  // inside `order()`, a refusal from the core — leaves the harness on
  // `files, radio, generic-input` for good. The next spec on this single
  // worker (`phone.spec.ts`'s last test, which reads the same `/api/status`
  // order through `usePlugins().admins`) would then fail too, as an
  // unrelated-looking **second** red test — exactly the shape Step 4 of this
  // task's own brief warns a reorder here has already produced once.
  try {
    await radioRow.locator('[data-plugin-down]').click()
    // Polled against the real core: a route that answered 202 without
    // writing anything, or a `move_entry` that silently no-oped, would leave
    // this unchanged, whatever the page renders in the meantime.
    await expect.poll(order).not.toEqual(before)
    const afterMove = await order()

    // Put it back: radio now sits one place later than it started, so its own
    // up arrow is the inverse of the gesture just made.
    await radioRow.locator('[data-plugin-up]').click()
    await expect.poll(order).toEqual(before)

    // The move actually swapped two adjacent names rather than, say,
    // dropping one — the same two names, reordered.
    expect([...afterMove].sort()).toEqual([...before].sort())
  } finally {
    // Restored through the API directly, not through the page: by the time
    // this runs, the page itself may be the thing that failed. A no-op
    // (`current` already equals `before`) when the happy path above already
    // completed successfully — `order()` is read fresh rather than assumed.
    const current = await order();
    if (JSON.stringify(current) !== JSON.stringify(before)) {
      const radioIndex = current.indexOf('radio')
      const delta = radioIndex > before.indexOf('radio') ? -1 : 1
      await request.post('/api/plugins/radio/move', { data: { delta } })
      await expect.poll(order).toEqual(before)
    }
  }
})
