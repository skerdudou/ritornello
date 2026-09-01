import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

/**
 * Fixtures root prepared by the harness, **as the core sees it**.
 *
 * It is drawn at random on every run (throwaway directory), so it cannot be
 * hard-coded here; and under Windows the core runs inside WSL, where the same
 * directory is called `/mnt/c/...`. Typing the Windows path into the page would
 * fail validation (`Roots::validate` wants an absolute path, and `C:\...` is
 * not one for Linux): so it is serve.mjs that publishes the useful form, in the
 * same state file that teardown.mjs uses.
 *
 * Read **inside the test** and not at module load: the file is only written
 * when the web server starts, which follows test collection.
 */
function fixturesRoot(): string {
  // Same computation as serve.mjs and teardown.mjs: `process.cwd()` is
  // `web/app` (npm puts the process there for a `-w app` script), the state
  // file lives at the repository root, under `target/`.
  const rootNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')
  const state = JSON.parse(
    readFileSync(join(rootNative, 'target', 'e2e-state.json'), 'utf8'),
  ) as { mediaRoot: string }
  return state.mediaRoot
}

// A single test, and deliberately so: each step relies on the server state
// left by the previous one (the declared root, then the scanned list, then the
// saved list). Splitting them into as many tests would make them dependent on
// their execution order without anything saying so.
test('files plugin journey: local root, scan, saved list, presets', async ({
  page,
  request,
}) => {
  // The scan is polled every second, the source change starts a real mpv
  // playback: the default 30 s margin is short for the whole.
  test.setTimeout(120_000)
  const root = fixturesRoot()

  // The three panels now live in tabs. They all stay mounted (`force-mount`,
  // so as not to lose the browser's open folder when switching tabs), hence
  // present in the DOM but hidden: Playwright refuses to click what is not
  // visible, and each step must open the tab it uses. The value, never the
  // label, which is translated.
  const openTab = async (name: 'list' | 'parcourir' | 'sources') => {
    await page.locator(`[data-tab="${name}"]`).click()
  }

  // --- The management page, on a truly empty sandbox -------------------------
  await page.goto('/plugins/files/')
  // The plugin's ESM module is loaded dynamically and resolved through the
  // import map: that is what no unit test can verify.
  // An open tab must **close** another. Reported in use: the panels stay
  // mounted (`force-mount`, so as not to lose the browser's open folder), and
  // reka-ui then sets no `hidden` -- it leaves the hiding to the consumer.
  // Without the class that takes care of it, the three panels showed up at
  // once and the tabs had no visible effect. Nothing but this journey can
  // catch it: jsdom has no layout engine, and the equivalent unit assertion
  // would wrongly pass.
  await expect(page.locator('[data-playlist-pane]')).toBeVisible()
  await expect(page.locator('[data-browse-pane]')).toBeHidden()
  await expect(page.locator('[data-sources-pane]')).toBeHidden()

  await openTab('sources')
  await expect(page.locator('[data-sources-pane]')).toBeVisible()
  await expect(page.locator('[data-playlist-pane]')).toBeHidden()
  // No source: the proof that the harness really redirected
  // `RITORNELLO_FILES_ROOTS` to its throwaway directory. Without that, a run on
  // a machine where Ritornello is installed would read — and overwrite — the
  // owner's `/etc/ritornello/media-roots.toml`.
  await expect(page.locator('[data-no-sources]')).toBeVisible()

  // --- Declare a folder of the device, through the wizard ---------------------
  // The heart of this work: we no longer type an absolute path that no screen
  // shows, we start from the mounted volumes. The harness *describes* one in a
  // fake /proc/mounts, for lack of being able to mount one without privilege.
  await page.locator('[data-add-device]').click()
  // The dialog content lives in a portal: Playwright sees it (it queries the
  // document), whereas a unit test's `wrapper.find` would not.
  await expect(page.locator('[data-volume]')).toHaveCount(1)
  await page.locator('[data-volume]').click()

  // The dialog must not overflow its own frame. `DialogContent` is a grid, and
  // the minimum width of a grid child defaults to that of its content: a long
  // folder name pushed the panel beyond its white background, and the
  // scrollbar as well as the buttons ended up painted outside. jsdom has no
  // layout engine and cannot see it — it is measurable here, and nowhere else.
  const dialog = page.locator('[data-device-dialog]')
  await expect(dialog.locator('[data-picker-folder]').first()).toBeVisible()
  const overflow = await dialog.evaluate(
    (el) => el.scrollWidth - el.clientWidth,
  )
  expect(overflow, 'the dialog overflows its frame horizontally').toBeLessThanOrEqual(1)
  // We descend into `media`, the fixtures folder. `proc` is not offered: the
  // single volume is the throwaway directory, and the filesystem whitelist
  // rules out pseudo-filesystems.
  await page.locator('[data-picker-folder]', { hasText: 'media' }).first().click()
  await expect(page.locator('[data-audio-count]')).toBeVisible()
  await page.locator('[data-choose]').click()
  // A refusal shows verbatim in `[data-message]`: requiring it absent gives a
  // failure that names the cause instead of a mere "no source".
  await expect(page.locator('[data-message]')).toHaveCount(0)
  await expect(page.locator('[data-source-row]')).toHaveCount(1)
  // Read back from the plugin, and not only displayed: it is the only way to
  // prove that the table really reached `media-roots.toml`, since the page
  // could show the row just chosen even if the save failed.
  const afterRoots = await (await request.get('/plugins/files/api/data')).json()
  expect(afterRoots.roots).toHaveLength(1)
  // The name is no longer typed: it is **derived** from the last path segment.
  expect(afterRoots.roots[0].name).toBe('media')
  expect(afterRoots.roots[0].path).toBe(root)
  // The password never travels to the browser — guaranteed by the type on the
  // plugin side (`Root` does not carry the field), verified here end to end.
  expect(JSON.stringify(afterRoots)).not.toContain('password')

  // --- Browse, then add the folder recursively --------------------------------
  // No reload here, and that is the regression this step pins down: the Browse
  // panel requests its first level from a watcher that fires **during** the
  // render triggered by the save. As long as the page's single in-flight
  // request also covered the read-back, this watcher received `null` and the
  // level stayed empty indefinitely — measured: `[data-browse-row]` stuck at 0
  // for the whole 5 s wait. The in-flight request now only covers the send.
  await openTab('browse')
  const rows = page.locator('[data-browse-row]')
  // A single level requested on opening: the root only contains `Album`, and
  // no audio file at its top.
  await expect(rows).toHaveCount(1)
  await expect(rows.first().locator('[data-browse-name]')).toHaveText('Album')
  // Entering the folder: the browser replaces the displayed level, it does not
  // unfold it underneath.
  await rows.first().locator('[data-browse-dir]').click()
  // Playlists come **before** the tracks: they are often what one looks for in
  // an album folder, and a list drowned under a hundred files goes unseen. And
  // `Album` has vanished from the screen: it is the previous level, replaced by
  // the one just opened.
  await expect(page.locator('[data-browse-name]')).toHaveText([
    'tout.m3u',
    '01.mp3',
    '02.mp3',
    '03.mp3',
  ])
  // "Add this folder": at the top of a source this button does not exist
  // (adding the whole source lives on its row, in the Sources panel), but once
  // inside `Album` it designates the open folder.
  await page.locator('[data-add-current]').click()
  // The scan is **asynchronous** on the plugin side: `add_dir` returns before
  // the walk ends, and the admin protocol pushes nothing. It is the page's
  // per-second polling that makes the tracks arrive — so we wait for the
  // observable state, never a fixed delay.
  await openTab('playlist')
  const tracks = page.locator('[data-track-row]')
  await expect(tracks).toHaveCount(3, { timeout: 30_000 })
  await expect(page.locator('[data-track-name]')).toHaveText(['01', '02', '03'])
  // No scan incident, and no missing track: the paths the plugin memorized are
  // read back from the process that wrote them.
  await expect(page.locator('[data-scan-error]')).toHaveCount(0)
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)

  // Durations are collected in the background, from the files' headers.
  //
  // A scan provides no duration — only an `#EXTINF` carries one — so the
  // column showed a dash. The plugin now reads the headers, in batches, and
  // the page polls for as long as it takes. The fixtures are 30 s long: that
  // is the value we must see arrive, which proves a real read and not an
  // invented value.
  await expect(page.locator('[data-track-row]').first().locator('td').nth(2)).toHaveText(
    /0:(29|30|31)/,
    { timeout: 30_000 },
  )

  // --- Save, clear, reload ----------------------------------------------------
  await expect(page.locator('[data-no-saved]')).toBeVisible()
  await page.locator('[data-playlist-name]').fill('journey')
  // The destination stays the internal storage: the fixtures root is not
  // declared writable, and the plugin would refuse to write there.
  await page.locator('[data-save-playlist]').click()
  await expect(page.locator('[data-saved-pick] option')).toHaveCount(1)
  await expect(page.locator('[data-saved-pick] option').first()).toContainText('journey')

  await page.locator('[data-clear]').click()
  await expect(tracks).toHaveCount(0)
  await expect(page.locator('[data-empty-playlist]')).toBeVisible()

  // --- Load an m3u found while browsing --------------------------------------
  // A playlist file placed on the source, with paths relative to itself. It
  // appears in the level **apart** from the tracks, and carries a different
  // action: it replaces the list instead of appending to it.
  // The open level is still `Album`, established by the journey step above:
  // nothing has changed it since (adding, saving, clearing and reloading the
  // list does not navigate elsewhere). Nor does the detour through the List
  // tab, and that is precisely what `force-mount` guarantees — without it the
  // panel would be remounted on the source root.
  await openTab('browse')
  const m3uRow = page
    .locator('[data-browse-row]')
    .filter({ has: page.locator('[data-browse-name]', { hasText: 'tout.m3u' }) })
  await expect(m3uRow).toHaveCount(1)
  // And above all not the add-a-track action: the right gesture must not be a
  // choice between two.
  await expect(m3uRow.locator('[data-add-file]')).toHaveCount(0)
  await m3uRow.locator('[data-load-m3u]').click()
  await openTab('playlist')
  await expect(tracks).toHaveCount(3)
  // The relative paths resolved against the m3u's directory: tracks marked
  // missing would signal a resolution against the wrong directory.
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)

  await page.locator('[data-clear]').click()
  await expect(tracks).toHaveCount(0)

  await page.locator('[data-load-playlist]').click()
  await expect(tracks).toHaveCount(3)
  // The paths survived the round trip through the internal m3u: a reloaded
  // list whose tracks were marked missing would signal relative paths where
  // the internal storage requires absolute ones.
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)
  await expect(page.locator('[data-track-name]')).toHaveText(['01', '02', '03'])

  // --- The home page: the grid follows the active source ----------------------
  await page.goto('/')
  await expect(page.locator('[data-source]')).toHaveText('radio')
  // `SourceCycle`: the core sorts the sources by name (`files`, `radio`) and
  // starts on the one `state.json` memorized — `radio`. One cycle therefore
  // leads to `files`, a second one brings it back.
  const source = page.getByRole('button', { name: 'Source', exact: true })
  await source.click()
  await expect(page.locator('[data-source]')).toHaveText('files')
  // Three tracks, three numbers: the count is declared by the Source half on
  // activation, and it is what arms the remote.
  await expect(page.locator('[data-preset-button]')).toHaveCount(3)
  await expect(page.locator('[data-preset-count]')).toContainText('3')
  // No pagination under ten presets.
  await expect(page.locator('[data-preset-prev]')).toHaveCount(0)
  await expect(page.locator('[data-preset-next]')).toHaveCount(0)
  // **And every tile carries a title**, like the radio's. The source only
  // declared a count, so the default body of `list_presets` returned an empty
  // list and the grid only showed bare numbers. The complete path is only seen
  // here: the source enumerates, the core holds the catalogue, `/api/presets`
  // serves it, the grid reads it.
  await expect(page.locator('[data-preset-name]')).toHaveText(['01', '02', '03'])

  // Choosing a track by its number must really go there.
  //
  // This is the central defect of this work, and only a journey with a real
  // mpv could see it: the core loaded the m3u with `loadfile`, which mpv only
  // unfolds **afterwards** (measured: `playlist-count` is 1, then 3 after an
  // `end-file`). The chained `playlist-pos` therefore fell out of bounds,
  // playback restarted from track 1, and the display lost number and name.
  // `loadlist` unfolds on the spot. No unit test could catch it: there is no
  // mpv in there.
  await page.locator('[data-preset-button]').nth(1).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('2')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('02')
  await page.locator('[data-preset-button]').nth(2).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('03')

  // Stop, then Play: playback resumes on the track being listened to.
  //
  // `stop` empties mpv's playlist, so that "toggle pause" had nothing left to
  // resume: the Play key did nothing at all, on every source. It now asks the
  // active source to play again.
  //
  // And a stop **keeps the track armed** on the display — number and name —
  // instead of leaving only a bare status: the display thus says "nothing is
  // playing, here is what will start again". That is what this step verifies
  // end to end.
  //
  // What it **cannot** distinguish: stopped or playing, the display is the same
  // on these fixtures (a sine wave has no metadata, hence no "now playing"
  // block to observe). The real discrimination is carried by the unit tests of
  // `stop()` and `PlayPause`.
  await page.getByRole('button', { name: 'Stop', exact: true }).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('03')
  await page.getByRole('button', { name: 'Play/Pause', exact: true }).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')

  // Progress, end to end: mpv measures, the core publishes one frame per
  // second, the SPA draws. No unit test covers this chain — there is no mpv in
  // there.
  //
  // These fixtures are sine waves **without any metadata**: in passing they
  // prove that the bar does not depend on a title. As long as it lived in the
  // "now playing" block, guarded by the presence of metadata, it was invisible
  // on a file without tags — that is, precisely when mpv knows the position
  // best.
  const position = page.locator('[data-position]')
  await expect(position).toBeVisible({ timeout: 15_000 })
  const first = await position.textContent()
  await expect(position).not.toHaveText(first ?? '', { timeout: 10_000 })
  // A local file is seekable: the bar is a slider, named and reachable from
  // the keyboard. The role and the aria-label live on the thumb, not on the
  // wrapper (see the kit's Slider.vue: the aria-label goes down to
  // `SliderThumb`, the only element a screen reader announces).
  const thumb = page.locator('[data-bar] [role="slider"]')
  await expect(thumb).toHaveCount(1)
  await expect(thumb).toHaveAttribute('aria-label', /.+/)

  // Dragging the bar, end to end: a real mouse gesture issues a `SeekTo`,
  // which mpv really applies. No unit test can catch it (see
  // `ProgressBar.test.ts`, simulated drag on the component) nor the phone
  // console (non-`seekable` source).
  // `data-bar` is set on the Slider root itself (see the kit's Slider.vue:
  // non-`aria-*` attrs forwarded to `SliderRoot`, which already carries
  // `data-slot="slider"`) — same pattern as `data-volume-slider` in
  // phone.spec.ts, not a descendant.
  const barTrack = page.locator('[data-bar]')
  const barBox = await barTrack.boundingBox()
  if (!barBox) throw new Error('progress track not visible')
  const yBar = barBox.y + barBox.height / 2
  const seekResponse = page.waitForResponse(
    (r) => r.url().endsWith('/api/command') && r.request().method() === 'POST',
  )
  // Same pattern as the volume slider (see phone.spec.ts): start from a point
  // on the track, not necessarily from the thumb itself.
  await page.mouse.move(barBox.x + barBox.width * 0.2, yBar)
  await page.mouse.down()
  await page.mouse.move(barBox.x + barBox.width * 0.5, yBar, { steps: 5 })
  await page.mouse.up()
  expect((await seekResponse).status()).toBe(204)
  // The 204 only proves the enqueueing (see the same comment in phone.spec.ts):
  // we probe a fresh SSE connection until a frame shows the seek really
  // applied by mpv, rather than assuming a fixed delay.
  const readProgressSse = () =>
    page.evaluate(
      () =>
        new Promise<{ position: number | null; duration: number | null }>((resolve, reject) => {
          const stream = new EventSource('/api/player')
          const timer = setTimeout(() => { stream.close(); reject(new Error('no frame within 2 s')) }, 2000)
          stream.onmessage = (e) => {
            clearTimeout(timer)
            stream.close()
            const frame = JSON.parse(e.data as string) as { position_s: number | null; duration_s: number | null }
            resolve({ position: frame.position_s, duration: frame.duration_s })
          }
        }),
    )
  let lastProgress: { position: number | null; duration: number | null } = { position: null, duration: null }
  await expect
    .poll(async () => {
      lastProgress = await readProgressSse()
      const { position, duration } = lastProgress
      if (position == null || duration == null || duration <= 0) return false
      return position >= duration * 0.4
    }, { timeout: 10_000 })
    .toBe(true)

  // Put the harness back in the state we found it: the journeys share a single
  // core and `files.spec.ts` runs **before** `journey.spec.ts`, which requires
  // the radio to be active. The restoration is verified, not hoped for.
  await source.click()
  await expect(page.locator('[data-source]')).toHaveText('radio')

  // --- The network wizard, without a NAS --------------------------------------
  // Last, and deliberately: declaring a share requires a mount reconciliation
  // that will fail here (neither polkit nor a systemd unit in the sandbox),
  // and that failure must above all not undo what the previous steps
  // established.
  //
  // The `smbclient` the plugin finds is the harness's, which returns outputs
  // **captured on a real NAS**. That is what makes this step playable on any
  // machine while testing the parsing against real data.
  await page.goto('/plugins/files/')
  // The reload brings back the default tab, the list.
  await openTab('sources')
  await page.locator('[data-add-share]').click()
  await page.locator('[data-host]').fill('192.168.1.15')
  await page.locator('[data-user]').fill('ritornello')
  await page.locator('[data-password]').fill('peu-importe')
  await page.locator('[data-connect]').click()
  // Two shares, not three: `IPC$` carries the type `IPC|` and not `Disk|`, and
  // the "SMB1 disabled" noise line is not a share either.
  await expect(page.locator('[data-share]')).toHaveCount(2, { timeout: 30_000 })
  await page.locator('[data-share]', { hasText: 'music' }).first().click()
  // A folder name with spaces survives the right-to-left parsing of `ls`: it is
  // the case that condemns any left-to-right read.
  await expect(page.locator('[data-picker-folder]', { hasText: 'Yann Tiersen' })).toHaveCount(1)
  await page.locator('[data-picker-folder]', { hasText: 'Yann Tiersen' }).click()
  await page.locator('[data-choose]').click()

  // The source is no longer declared when the mount fails: the declaration is
  // undone entirely (table, credentials file), and the refusal surfaces in the
  // dialog rather than closing it — losing the input because a NAS is asleep
  // would be the worst response. A single source remains: the local folder
  // established at the beginning of the journey.
  await expect(page.locator('[data-source-row]')).toHaveCount(1)
  // The refusal shows verbatim in the dialog, not only on the page (behind its
  // grey veil, the page banner would be invisible at the moment it matters).
  await expect(page.locator('[data-dlg-message]')).toContainText(
    'the share was not mounted, so it has not been declared',
  )
  // The dialog stays open, input included: nothing forces retyping everything.
  // Both fields, and not only the host: the promise covers the whole input.
  await expect(page.locator('[data-share-dialog]')).toBeVisible()
  await expect(page.locator('[data-host]')).toHaveValue('192.168.1.15')
  await expect(page.locator('[data-user]')).toHaveValue('ritornello')
  const afterShare = await (await request.get('/plugins/files/api/data')).json()
  expect(afterShare.roots).toHaveLength(1)
  // And the password never reached the page, even in this refusal.
  expect(JSON.stringify(afterShare)).not.toContain('peu-importe')
})
