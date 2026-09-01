import { flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mountAdmin } from './harness'

const THREE = [
  { path: 'Albums/Jazz/01.mp3', name: 'Track 1', duration_s: 245, missing: false },
  { path: 'Albums/Jazz/02.mp3', name: 'Track 2', duration_s: 0, missing: true },
  { path: 'Albums/Jazz/03.mp3', name: 'Track 3', duration_s: 3725, missing: false },
]

/**
 * Stand-in for the core's pushed stream: jsdom has no `EventSource`.
 *
 * Must be installed **before** mounting — the page subscribes to it in
 * `onMounted`, and a stand-in put in place afterwards would never be seen.
 */
function playerStream() {
  const relay: { send: ((e: MessageEvent) => void) | null } = { send: null }
  vi.stubGlobal(
    'EventSource',
    class {
      set onmessage(f: (e: MessageEvent) => void) {
        relay.send = f
      }
      close(): void {}
    },
  )
  return {
    push: (state: unknown) => {
      relay.send?.({ data: JSON.stringify(state) } as MessageEvent)
    },
  }
}

describe('current queue pane', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('flags a missing track without ever hiding it', async () => {
    // Encoded regression: a list that shrinks on its own is a defect that
    // takes months to attribute. An unmounted share, on the other hand, is
    // diagnosed in one second as long as the tracks stay displayed, flagged.
    const { w } = await mountAdmin({ playlist: THREE })
    expect(w.findAll('[data-track-row]')).toHaveLength(3)
    expect(w.findAll('[data-track-missing]')).toHaveLength(1)
    expect(w.findAll('[data-track-name]')[1]!.text()).toBe('Track 2')
    // The full path is in the tooltip: it is what says *which* file is
    // missing, the name alone is not enough to find it back on the share.
    expect(w.findAll('[data-track-missing]')[0]!.attributes('title')).toBe('Albums/Jazz/02.mp3')
  })

  it('does not blame a track whose mount was not responding', async () => {
    // `missing: null` means "unknown": the plugin could not check, its
    // circuit breaker having tripped on a silent share. Displaying "missing"
    // would blame the file for a failure that is the mount's — and would
    // send the user searching for a file that is right there.
    const { w } = await mountAdmin({
      playlist: [{ path: '/mnt/ritornello/nas/a.mp3', name: 'On the NAS', duration_s: 0, missing: null }],
      unresponsive: ['/mnt/ritornello/nas'],
    })
    expect(w.findAll('[data-track-missing]')).toHaveLength(0)
    const unknown = w.findAll('[data-track-unknown]')
    expect(unknown).toHaveLength(1)
    expect(unknown[0]!.attributes('title')).toBe('/mnt/ritornello/nas/a.mp3')
    // The track stays there, like a missing track: it is lists that shrink
    // silently that cost months to diagnose.
    expect(w.findAll('[data-track-row]')).toHaveLength(1)
  })

  it('renders durations, including a dash for an unknown one', async () => {
    const { w } = await mountAdmin({ playlist: THREE })
    const text = w.find('[data-playlist-pane]').text()
    expect(text).toContain('4:05')
    expect(text).toContain('1:02:05')
    expect(text).toContain('—')
  })

  it('reorders, removes and clears using absolute indices', async () => {
    const { w, s } = await mountAdmin({ playlist: THREE })
    await w.findAll('[data-track-down]')[0]!.trigger('click')
    await flushPromises()
    expect(s.putsOf('move')).toEqual([{ op: 'move', from: 0, to: 1 }])

    await w.findAll('[data-track-remove]')[2]!.trigger('click')
    await flushPromises()
    expect(s.putsOf('remove')).toEqual([{ op: 'remove', index: 2 }])

    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsOf('clear')).toEqual([{ op: 'clear' }])
  })

  it('bounds the arrows at the ends of the list', async () => {
    const { w } = await mountAdmin({ playlist: THREE })
    expect((w.findAll('[data-track-up]')[0]!.element as HTMLButtonElement).disabled).toBe(true)
    expect((w.findAll('[data-track-down]')[2]!.element as HTMLButtonElement).disabled).toBe(true)
  })

  it('clearing during playback also asks the core to stop', async () => {
    // Reported design defect: the Admin half cannot ask mpv anything — SDK
    // notifications carry no action — so clearing left the music playing on
    // a now-empty list. It is the page that requests the stop, through the
    // same channel as the remote: a user gesture.
    const { w, s } = await mountAdmin({ playlist: THREE, playing: true })
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsOf('clear')).toHaveLength(1)
    expect(s.urls()).toContain('/api/command')
  })

  it('requests the stop even when the page wrongly believed nothing was playing', async () => {
    // Measured fragility: the page does not poll continuously, so `playing`
    // can be stale. Reading it before clearing silenced the stop request
    // without anything signalling it. So the state read is the one
    // **rendered by the clearing**, which does not touch `playing`.
    const { w, s } = await mountAdmin({ playlist: THREE, playing: false })
    s.onPut = () => {
      s.data.playing = true
    }
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.urls()).toContain('/api/command')
  })

  it('requests the stop when the core is playing this source, even if the plugin does not know it', async () => {
    // Reported defect: at startup, the plugin's flag stays false — mpv
    // briefly goes idle before loading the first file, and the core then
    // sends a `stop()` that clears it. The active source, on the other hand,
    // comes from the **core** via the pushed stream, and therefore cannot
    // drift.
    //
    // The expected name is that of `BASE` (`mediatheque`), not "files": it
    // is the deployment that names a plugin, and the page derives it from
    // its own prefix instead of hardcoding it.
    const stream = playerStream()
    const { w, s } = await mountAdmin({ playlist: THREE, playing: false })
    stream.push({ source: 'mediatheque' })
    await flushPromises()

    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.urls()).toContain('/api/command')
  })

  it('cuts nothing when the core is playing another source', async () => {
    // Guard rail: clearing a files list while the radio is playing must not
    // silence it.
    const stream = playerStream()
    const { w, s } = await mountAdmin({ playlist: THREE, playing: false })
    stream.push({ source: 'radio' })
    await flushPromises()

    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.urls()).not.toContain('/api/command')
  })

  it('clearing an idle list does not cut the source that is playing', async () => {
    // Without this condition, clearing an inactive files list would cut the
    // radio — the core's `Stop` applies to the active source, not to ours.
    const { w, s } = await mountAdmin({ playlist: THREE, playing: false })
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsOf('clear')).toHaveLength(1)
    expect(s.urls()).not.toContain('/api/command')
  })

  it('reorders via drag-and-drop, like the station grid', async () => {
    const { w, s } = await mountAdmin({ playlist: THREE })
    const rows = w.findAll('[data-track-row]')
    await rows[2]!.trigger('dragstart')
    await rows[0]!.trigger('drop')
    await flushPromises()
    expect(s.putsOf('move')).toEqual([{ op: 'move', from: 2, to: 0 }])
  })

  it('dropping a track onto itself requests nothing', async () => {
    // Otherwise the slightest heavy-handed click on a row would trigger a
    // no-op move, and a full state re-read along with it.
    const { w, s } = await mountAdmin({ playlist: THREE })
    const rows = w.findAll('[data-track-row]')
    await rows[1]!.trigger('dragstart')
    await rows[1]!.trigger('drop')
    await flushPromises()
    expect(s.putsOf('move')).toEqual([])
  })

  it('dropping without having picked anything up requests nothing', async () => {
    const { w, s } = await mountAdmin({ playlist: THREE })
    await w.findAll('[data-track-row]')[0]!.trigger('drop')
    await flushPromises()
    expect(s.putsOf('move')).toEqual([])
  })

  it('the ranks sent to the plugin are absolute, not the pages own', async () => {
    // Beyond two hundred tracks the list is paginated: confusing the
    // displayed rank with the real index would move an entirely different
    // track.
    const long = Array.from({ length: 250 }, (_, i) => ({
      path: `/m/${i}.mp3`,
      name: `${i}`,
      duration_s: 0,
      missing: false,
    }))
    const { w, s } = await mountAdmin({ playlist: long })
    await w.find('[data-page-next]').trigger('click')
    await flushPromises()
    const rows = w.findAll('[data-track-row]')
    await rows[1]!.trigger('dragstart')
    await rows[0]!.trigger('drop')
    await flushPromises()
    expect(s.putsOf('move')).toEqual([{ op: 'move', from: 101, to: 100 }])
  })

  it('displays what a loaded m3u could not resolve', async () => {
    // Without this box, the loaded list is simply shorter than the file,
    // with nothing saying so.
    const { w } = await mountAdmin({
      playlist: THREE,
      unresolved: ['Albums/Rock/lost.mp3', 'Albums/Rock/also.mp3'],
    })
    const box = w.find('[data-unresolved]')
    expect(box.text()).toContain('2 entries could not be found')
    expect(box.findAll('[data-unresolved-row]').map((r) => r.text())).toEqual([
      'Albums/Rock/lost.mp3',
      'Albums/Rock/also.mp3',
    ])
  })

  it('shows no box when everything was resolved', async () => {
    const { w } = await mountAdmin({ playlist: THREE })
    expect(w.find('[data-unresolved]').exists()).toBe(false)
  })

  it('paginates beyond two hundred tracks, without losing any', async () => {
    // Rendering several thousand rows at once freezes the tab for several
    // seconds on a Raspberry Pi's browser.
    const long = Array.from({ length: 250 }, (_, i) => ({
      path: `p/${i}.mp3`,
      name: `Track ${i}`,
      duration_s: 100,
      missing: false,
    }))
    const { w } = await mountAdmin({ playlist: long })
    expect(w.findAll('[data-track-row]')).toHaveLength(100)
    expect(w.find('[data-page-label]').text()).toBe('1–100 of 250')
    expect(w.findAll('[data-track-num]')[0]!.text()).toBe('1')

    await w.find('[data-page-next]').trigger('click')
    await w.find('[data-page-next]').trigger('click')
    // Last page: the remaining fifty, numbered from their real rank.
    expect(w.findAll('[data-track-row]')).toHaveLength(50)
    expect(w.find('[data-page-label]').text()).toBe('201–250 of 250')
    expect(w.findAll('[data-track-num]')[0]!.text()).toBe('201')
    expect((w.find('[data-page-next]').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('opens the paginated list on the page of the current track', async () => {
    // Landing on page 1 of a thousand-title list while the player is at the
    // 350th helps no one.
    const long = Array.from({ length: 1000 }, (_, i) => ({
      path: `p/${i}.mp3`,
      name: `Track ${i}`,
      duration_s: 100,
      missing: false,
    }))
    const { w } = await mountAdmin({ playlist: long, index: 349 })
    expect(w.find('[data-page-label]').text()).toBe('301–400 of 1000')
  })

  it('does not paginate a short list', async () => {
    const { w } = await mountAdmin({ playlist: THREE })
    expect(w.find('[data-page-label]').exists()).toBe(false)
  })

  it('saves the list under a name and a destination', async () => {
    const { w, s } = await mountAdmin({
      playlist: THREE,
      roots: [
        { name: 'nas', kind: 'smb', host: 'h', share: 's', writable: true },
        { name: 'read-only', kind: 'smb', host: 'h', share: 's', writable: false },
      ],
    })
    // Only the **writable** roots are offered: offering a share mounted
    // read-only would only produce a refusal from the plugin.
    const options = w.findAll('[data-playlist-where] option').map((o) => o.attributes('value'))
    expect(options).toEqual(['internal', 'nas'])

    await w.find('[data-playlist-name]').setValue('Jazz')
    await w.find('[data-playlist-where]').setValue('nas')
    await w.find('[data-save-playlist]').trigger('click')
    await flushPromises()
    expect(s.putsOf('save_playlist')).toEqual([
      { op: 'save_playlist', name: 'Jazz', where: 'nas' },
    ])
  })

  it('saves nothing without a list name', async () => {
    const { w, s } = await mountAdmin({ playlist: THREE })
    await w.find('[data-playlist-name]').setValue('   ')
    await w.find('[data-save-playlist]').trigger('click')
    await flushPromises()
    expect(s.putsOf('save_playlist')).toHaveLength(0)
  })

  it('loads a saved list from its original location', async () => {
    // The name + location pair is what identifies a list: two "Jazz" lists
    // can coexist, one internal, the other on the share.
    const { w, s } = await mountAdmin({
      saved: [
        { name: 'Jazz', where: 'internal' },
        { name: 'Jazz', where: 'nas' },
      ],
    })
    expect(w.findAll('[data-saved-pick] option').map((o) => o.text().trim())).toEqual([
      'Jazz — internal storage',
      'Jazz — nas',
    ])
    await w.find('[data-saved-pick]').setValue('1')
    await w.find('[data-load-playlist]').trigger('click')
    await flushPromises()
    expect(s.putsOf('load_playlist')).toEqual([
      { op: 'load_playlist', name: 'Jazz', where: 'nas' },
    ])
  })

  it('says so when there is no saved list', async () => {
    const { w } = await mountAdmin()
    expect(w.find('[data-no-saved]').text()).toBe('No saved playlist')
    expect(w.find('[data-empty-playlist]').text()).toBe('Empty queue')
  })
})
