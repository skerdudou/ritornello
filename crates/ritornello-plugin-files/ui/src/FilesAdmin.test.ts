import { SKELETON_DELAY_MS } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOG, mountAdmin, server } from './harness'

describe('FilesAdmin, the page', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(() => vi.useRealTimers())

  it('probes during the duration survey, and announces it', async () => {
    // Durations arrive in batches, in the background: without this probe the
    // column would stay at "—" until the user's next gesture. And saying it
    // matters — on a slow share, a list that changes before one's eyes without
    // explanation is worrying.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = server({
      playlist: [{ path: '/m/1.mp3', name: '01' }],
      durations: { running: true, done: 10, total: 40 },
    })
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.find('[data-durations]').text()).toBe('Reading lengths (10 of 40)')
    const gets = () => s.spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method !== 'PUT').length
    expect(gets()).toBe(1)

    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(2)

    // Survey finished: the probe stops and the announcement disappears.
    s.data.durations = { running: false, done: 40, total: 40 }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(w.find('[data-durations]').exists()).toBe(false)
    vi.advanceTimersByTime(5000)
    await flushPromises()
    expect(gets()).toBe(3)
  })

  it('re-reads the state when the player changes, so that the highlighted track follows', async () => {
    // Reported defect: the highlight comes from `index`, which only `api/data`
    // carries — and the probe stops as soon as no work is in progress. Since
    // the track changes by itself at every end of track, the highlight stayed
    // frozen on the one from the start.
    //
    // A pushed stream rather than a permanent probe: the core already announces
    // every change. jsdom has no `EventSource`, so it is simulated — otherwise
    // this path would be exercised nowhere.
    const stream: { onmessage: (() => void) | null } = { onmessage: null }
    vi.stubGlobal(
      'EventSource',
      class {
        onmessage: (() => void) | null = null
        constructor() {
          // eslint-disable-next-line @typescript-eslint/no-this-alias
          const self = this
          Object.defineProperty(stream, 'onmessage', {
            configurable: true,
            get: () => self.onmessage,
            set: (v) => {
              self.onmessage = v
            },
          })
        }
        close(): void {}
      },
    )
    const s = server({ playlist: [{ path: '/m/1.mp3', name: '01' }], index: 0 })
    mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    const before = s.urls().length

    s.data.index = 1
    stream.onmessage?.()
    await flushPromises()
    expect(s.urls().length).toBe(before + 1)
  })

  it('addresses all its requests under the absolute prefix received through `base`', async () => {
    // Encoded regression: a relative `./api/data` resolves against the browser
    // URL, not against the plugin prefix. On `/plugins/files` (without trailing
    // slash, a form the shell router also accepts) it would designate
    // `/plugins/api/data` — which the core interprets as a plugin named "api":
    // 404, empty page, every button failing.
    const { w, s } = await mountAdmin({ saved: [{ name: 'Jazz', where: 'internal' }] })
    await w.find('[data-load-playlist]').trigger('click')
    await flushPromises()
    expect(s.urls().length).toBeGreaterThan(1)
    for (const u of s.urls()) expect(u).toBe(`${BASE}api/data`)
  })

  it('displays a server refusal verbatim, without rewording it', async () => {
    // Refusals are produced by the **server's** i18n catalogs: they are already
    // translated, and substituting a home-made message for them would lose the
    // detail (root name, exceeded cap) that makes them actionable.
    const { w, s } = await mountAdmin({ playlist: [{ path: 'a.mp3', name: 'A' }] })
    s.refusal = 'invalid root name "My NAS": lowercase letters, digits and dashes only'
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(w.find('[data-message]').text()).toBe(s.refusal)
  })

  it('displays the systemctl output as is, line breaks included', async () => {
    // The failure of `{"op":"mount"}` carries the raw output of `systemctl`:
    // that is what is actionable. Folding it into a paragraph would make it
    // unreadable, and rewording it would destroy it — hence the render in a
    // `pre`.
    const output =
      'Job for ritornello-media-mount.service failed.\n' +
      'See "systemctl status ritornello-media-mount.service" and "journalctl -xeu ...".'
    // The retry only exists if a mount has already failed: the mount now
    // follows the declaration, there is no permanent "Mount" button to go
    // looking for anymore.
    const { w, s } = await mountAdmin({
      roots: [{ name: 'nas', kind: 'smb', host: 'h', share: 's', mounted: false }],
      mount_error: output,
    })
    s.refusal = output
    await w.find('[data-retry-mount]').trigger('click')
    await flushPromises()
    const pre = w.find('[data-message]')
    expect(pre.element.tagName).toBe('PRE')
    expect(pre.element.textContent).toBe(output)
  })

  it('probes during a scan and stops as soon as it finishes', async () => {
    // The admin protocol pushes **nothing**: neither an event channel nor a
    // websocket behind the admin socket. Without this probe, an `add_dir` —
    // asynchronous on the plugin side — would never display its progress, and
    // the list would only appear at the next manual reload of the page.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = server({ scan: { running: true, found: 12, dir: 'Albums/Jazz' } })
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.find('[data-scan]').text()).toBe('Scanning Albums/Jazz — 12 tracks found so far')
    const gets = () => s.spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method !== 'PUT').length
    expect(gets()).toBe(1)

    s.data.scan = { running: true, found: 300, dir: 'Albums/Rock' }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(2)
    expect(w.find('[data-scan]').text()).toContain('300')

    // End of the scan: the probe must stop by itself, otherwise the page
    // hammers the plugin once per second until it is closed.
    s.data.scan = { running: false, found: 300, dir: '' }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(3)
    expect(w.find('[data-scan]').exists()).toBe(false)

    vi.advanceTimersByTime(5000)
    await flushPromises()
    expect(gets()).toBe(3)
  })

  it('also probes during a connection to a share', async () => {
    // Regression found by the end-to-end journey, and by it alone: the SMB
    // connection is asynchronous on the plugin side — a powered-off NAS would
    // exceed the core's 5 s cap — but the probe only watched the scan. So the
    // dialog stayed stuck on "Connecting…" indefinitely, while the plugin had
    // answered long ago: nobody was reading it back anymore.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = server({ explore: { open: true, kind: 'smb', busy: true, host: 'nas' } })
    mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    const gets = () => s.spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method !== 'PUT').length
    expect(gets()).toBe(1)

    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(2)

    // Connection finished: the probe stops, as after a scan.
    s.data.explore = { open: true, kind: 'smb', busy: false, host: 'nas', shares: ['music'] }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(3)
    vi.advanceTimersByTime(5000)
    await flushPromises()
    expect(gets()).toBe(3)
  })

  it('shows the incident of the last scan, which survives its end', async () => {
    // Encoded regression: `add_dir` returns **before** the end of the recursive
    // walk, so its acknowledgement says nothing about its outcome. If the page
    // did not display `scan.error`, an addition that failed would pass for an
    // addition that simply found nothing.
    const refusal = 'this folder holds more than 10000 tracks: narrow it down'
    const { w } = await mountAdmin({ scan: { running: false, found: 0, dir: '', error: refusal } })
    expect(w.find('[data-scan-error]').element.textContent).toBe(refusal)
    expect(w.find('[data-scan]').exists()).toBe(false)
  })

  it('emits nothing more after unmounting, even in the middle of a scan', async () => {
    // Without `onUnmounted`, the timer survives the component: the shell
    // changes page and a `reload()` keeps running every second against a dead
    // component.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = server({ scan: { running: true, found: 1, dir: 'a' } })
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    const before = s.spy.mock.calls.length
    w.unmount()
    vi.advanceTimersByTime(5000)
    await flushPromises()
    expect(s.spy.mock.calls.length).toBe(before)
  })

  it('first load failed: the page is inert and writes nothing', async () => {
    // Encoded regression, of the same order as the radio page's: after a
    // failed GET, `roots` is empty while `media-roots.toml` is not. A "Save
    // roots" would send `{op:'save_roots', roots: []}`, which overwrites the
    // file — every declared share disappears, without confirmation or way
    // back.
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(null, { status: 204 })
      return new Response('unavailable', { status: 503 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.find('[data-message]').text()).toContain('Error: ')
    // The panes are not even mounted: there is nothing true to show.
    expect(w.find('[data-sources-pane]').exists()).toBe(false)
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('the failure of a probe does not make an already loaded page inert', async () => {
    // The guard only targets the first load: later, the data is there and does
    // not lie. Freezing the page because a one-second refresh failed would be a
    // loss of comfort without safety gain.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = server({
      scan: { running: true, found: 1, dir: 'a' },
      playlist: [{ path: 'a.mp3', name: 'A' }],
    })
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    s.spy.mockImplementationOnce(async () => new Response('unavailable', { status: 503 }))
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(w.find('[data-message]').text()).toContain('Error: ')
    expect((w.find('[data-clear]').element as HTMLButtonElement).disabled).toBe(false)
  })

  it('single flight: two operations launched back to back emit only one', async () => {
    // The SDK serves admin requests strictly serially and the core gives up
    // after 5 s: the second, queued behind the first, would exceed the cap and
    // receive the translated sentence from the core's catalog
    // (`plugin_timeout`) for a perfectly legitimate action.
    let release: () => void = () => {}
    const inProgress = new Promise<void>((r) => (release = r))
    const s = server({ playlist: [{ path: 'a.mp3', name: 'A' }] })
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    s.spy.mockImplementationOnce(async () => {
      await inProgress
      return new Response(null, { status: 204 })
    })
    await w.find('[data-clear]').trigger('click')
    await w.find('[data-clear]').trigger('click')
    expect(s.putsOf('clear')).toHaveLength(1)
    release()
    await flushPromises()
    // The state is restored: a new operation becomes possible again.
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsOf('clear')).toHaveLength(2)
  })

  it('presents the current playlist before the two other panes', async () => {
    // The order is that of usage: one looks at what is playing, then completes.
    // Declaring a source is rare, browsing comes after having seen the list.
    const { w } = await mountAdmin({ roots: [{ name: 'nas', kind: 'local', path: '/m' }] })
    const order = w
      .findAll('[data-playlist-pane],[data-browse-pane],[data-sources-pane]')
      .map((s) => Object.keys(s.attributes()).find((a) => a.endsWith('-pane')))
    expect(order).toEqual(['data-playlist-pane', 'data-browse-pane', 'data-sources-pane'])
  })

  it('arranges the three panes in tabs, the playlist open first', async () => {
    // The three panes end to end made a page one had to scroll through at
    // length to reach the declaration of a source, although it is almost never
    // touched.
    const { w } = await mountAdmin({ roots: [{ name: 'nas', kind: 'local', path: '/m' }] })
    const tabs = w.findAll('[data-tab]')
    expect(tabs.map((o) => o.text())).toEqual(['Current queue', 'Browse', 'Sources'])
    expect(tabs[0]!.attributes('data-state')).toBe('active')
    expect(tabs[1]!.attributes('data-state')).toBe('inactive')
  })

  it('switches tab without unmounting the other panes', async () => {
    // `force-mount` on the panels, and that is what this test protects: without
    // it, coming back to Browse after a detour through the playlist would
    // reopen the root of the source, losing the folder we were in — and would
    // relaunch a `browse` at every back and forth.
    const { w, s } = await mountAdmin({ roots: [{ name: 'nas', kind: 'local', path: '/m' }] })
    const browseBefore = s.putsOf('browse').length
    const browseTab = w.findAll('[data-tab]')[1]!
    ;(browseTab.element as HTMLElement).focus()
    await browseTab.trigger('click')
    await flushPromises()
    expect(browseTab.attributes('data-state')).toBe('active')
    // The Playlist pane is still mounted, simply hidden.
    expect(w.find('[data-playlist-pane]').exists()).toBe(true)
    // And no further browse was requested on tab change.
    expect(s.putsOf('browse').length).toBe(browseBefore)
  })

  // --- The first render, before any answer -------------------------------

  describe('while the first answer is still in flight', () => {
    /** Mounts with a `fetch` that never answers. */
    function mountUnanswered() {
      vi.stubGlobal(
        'fetch',
        vi.fn(() => new Promise<Response>(() => {})),
      )
      return mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    }

    it('shows nothing at all while the wait is short', async () => {
      // This page showed a strictly empty screen until the answer landed, then
      // the three tabs and their content appeared at once. The empty frame is
      // kept — it is the right thing for a fast load — and only the long wait
      // now gets an explanation.
      vi.useFakeTimers()
      const w = mountUnanswered()
      await flushPromises()
      vi.advanceTimersByTime(SKELETON_DELAY_MS - 1)
      await flushPromises()

      expect(w.find('[data-slot="skeleton"]').exists()).toBe(false)
      expect(w.find('[data-tab="playlist"]').exists()).toBe(false)
    })

    it('announces the wait once it outlasts the delay', async () => {
      vi.useFakeTimers()
      const w = mountUnanswered()
      await flushPromises()
      vi.advanceTimersByTime(SKELETON_DELAY_MS)
      await flushPromises()

      expect(w.find('[data-slot="skeleton"]').exists()).toBe(true)
      expect(w.get('[role="status"]').text()).toBe('Loading…')
    })

    it('gives up the placeholder when the load fails', async () => {
      // `data` stays null on a refusal, so a placeholder tied to it alone
      // would pulse for ever — on top of the very message explaining why the
      // page is inert.
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => new Response('nope', { status: 500 })),
      )
      const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
      await flushPromises()

      expect(w.find('[data-slot="skeleton"]').exists()).toBe(false)
      expect(w.get('[data-message]').text()).toContain('Error: ')
    })
  })
})
