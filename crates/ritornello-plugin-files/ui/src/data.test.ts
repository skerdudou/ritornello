import { describe, expect, it } from 'vitest'
import {
  rootTarget,
  leaf,
  formatDuration,
  normalizeBrowse,
  normalizeData,
  normalizeRoot,
  truncateStart,
} from './data'

describe('root normalization', () => {
  it('fills in fields the plugin omits when they are empty', () => {
    // Encoded regression: `Root` is serialized with
    // `skip_serializing_if = "Option::is_none"`, so `subpath` **vanishes**
    // from the body instead of appearing empty in it. A view that called
    // `r.subpath.trim()` would throw inside a `computed`, and a `computed`
    // that throws leaves the page half-rendered, with no message.
    const r = normalizeRoot({ name: 'nas', kind: 'smb', host: 'h', share: 's' })
    expect(r.subpath).toBe('')
    expect(r.user).toBe('')
    expect(r.writable).toBe(false)
    expect(r.mounted).toBe(false)
  })

  it('treats any unknown kind as a share', () => {
    expect(normalizeRoot({ kind: 'local' }).kind).toBe('local')
    expect(normalizeRoot({}).kind).toBe('smb')
  })
})

describe('browse normalization', () => {
  it('reassembles the path of each entry, since the plugin only renders names', () => {
    // Encoded regression: `scan::list_dir` renders `dirs` and `files` as
    // **plain names**, relative to the directory read. A page that took
    // them for paths would send back `Jazz` instead of `Albums/Jazz` in the
    // next `browse` — hence a different folder, or a refusal.
    const nav = normalizeBrowse({
      root: 'nas',
      path: 'Albums',
      dirs: ['Jazz'],
      files: ['01.mp3'],
      results: [],
    })
    expect(nav.entries).toEqual([
      { name: 'Jazz', path: 'Albums/Jazz', dir: true, playlist: false },
      { name: '01.mp3', path: 'Albums/01.mp3', dir: false, playlist: false },
    ])
  })

  it('prefixes nothing at the top level, whose path is empty', () => {
    const nav = normalizeBrowse({ root: 'nas', path: '', dirs: ['Albums'], files: [] })
    expect(nav.entries).toEqual([{ name: 'Albums', path: 'Albums', dir: true, playlist: false }])
  })

  it('takes search results for full paths', () => {
    // Unlike a level: a search walks the whole tree, its finds are not in
    // the current directory.
    const nav = normalizeBrowse({
      root: 'nas',
      path: '',
      dirs: [],
      files: [],
      results: ['Albums/Jazz/miles.flac'],
      truncated: true,
    })
    expect(nav.results).toEqual([
      { name: 'miles.flac', path: 'Albums/Jazz/miles.flac', dir: false, playlist: false },
    ])
    expect(nav.truncated).toBe(true)
  })

  it('renders an empty browse rather than throwing on a missing field', () => {
    expect(normalizeBrowse(undefined).entries).toEqual([])
    expect(normalizeBrowse({}).truncated).toBe(false)
  })

  it('places playlists before tracks, and flags them as such', () => {
    // They carry a different action — load, not add — and a playlist
    // drowned under a hundred files goes unnoticed, while it is often what
    // is being looked for in an album folder.
    const nav = normalizeBrowse({
      root: 'nas',
      path: 'Albums',
      dirs: ['Jazz'],
      files: ['01.mp3'],
      playlists: ['tout.m3u'],
      results: [],
    })
    expect(nav.entries.map((e) => e.name)).toEqual(['Jazz', 'tout.m3u', '01.mp3'])
    expect(nav.entries[1]).toEqual({
      name: 'tout.m3u',
      path: 'Albums/tout.m3u',
      dir: false,
      playlist: true,
    })
  })

  it('an older plugin, without the playlists field, breaks nothing', () => {
    // During a deployment, the binary can be ahead of the page or the
    // reverse.
    const nav = normalizeBrowse({ root: 'n', path: '', dirs: [], files: ['a.mp3'] })
    expect(nav.entries.map((e) => e.playlist)).toEqual([false])
  })

  it('keeps the search pattern, empty for a browse', () => {
    expect(normalizeBrowse({ root: 'nas', path: 'A', query: 'miles' }).query).toBe('miles')
    expect(normalizeBrowse({ root: 'nas', path: 'A' }).query).toBe('')
  })
})

describe('full payload normalization', () => {
  it('accepts a minimal body without throwing', () => {
    const d = normalizeData({})
    expect(d.roots).toEqual([])
    expect(d.playlist).toEqual([])
    expect(d.scan).toEqual({ running: false, found: 0, dir: '', error: '' })
    expect(d.unresolved).toEqual([])
  })

  it('carries over the last scan incident, which survives its own end', () => {
    // `add_dir` returns well before the recursive walk finishes: it is the
    // only place where the page can learn that an addition failed.
    const d = normalizeData({
      scan: { running: false, found: 0, dir: '', error: 'could not read "Albums"' },
    })
    expect(d.scan.error).toBe('could not read "Albums"')
  })

  it('falls a nameless track back to the last segment of its path', () => {
    const d = normalizeData({ playlist: [{ path: 'Albums/Jazz/01.mp3' }] })
    expect(d.playlist[0]).toEqual({
      path: 'Albums/Jazz/01.mp3',
      name: '01.mp3',
      duration_s: 0,
      // `null` and not `false`: a payload that says nothing about the
      // file's existence does not allow asserting that it is there.
      // Defaulting to "present" would display a track as healthy when the
      // plugin was never able to look at it — that is the lie the
      // three-state field removes.
      missing: null,
    })
  })
})

describe('formatting', () => {
  it('renders an unknown duration as a dash, never as "0:00"', () => {
    // "0:00" would assert an empty track; the dash says it is not known.
    expect(formatDuration(0)).toBe('—')
    expect(formatDuration(Number.NaN)).toBe('—')
  })

  it('switches to hours beyond sixty minutes', () => {
    expect(formatDuration(245)).toBe('4:05')
    expect(formatDuration(3725)).toBe('1:02:05')
  })

  it('composes a root target according to its kind', () => {
    expect(rootTarget(normalizeRoot({ kind: 'local', path: '/mnt/usb' }))).toBe('/mnt/usb')
    expect(
      rootTarget(normalizeRoot({ kind: 'smb', host: 'nas', share: 'musique' })),
    ).toBe('//nas/musique')
    expect(
      rootTarget(
        normalizeRoot({ kind: 'smb', host: 'nas', share: 'musique', subpath: 'Albums' }),
      ),
    ).toBe('//nas/musique/Albums')
  })

  it('extracts the last segment of a path', () => {
    expect(leaf('a/b/c.mp3')).toBe('c.mp3')
    expect(leaf('')).toBe('')
  })
})

describe('truncating a path from the start', () => {
  it('leaves a path that fits untouched', () => {
    expect(truncateStart('/media/usb/Albums', 52)).toBe('/media/usb/Albums')
    expect(truncateStart('', 52)).toBe('')
  })

  it('cuts the start and keeps the end, which is the useful information', () => {
    // That is the whole point of this function: on a path, what matters is
    // the folder currently in, not the root it came from. `text-overflow`
    // only knows how to cut on the right, so it would do exactly the
    // opposite.
    const path = '/mnt/c/Users/skerdudou/OneDrive - Klee Group/perso/steven prive/mp3'
    const short = truncateStart(path, 30)
    expect(short.startsWith('…/')).toBe(true)
    expect(short.endsWith('mp3')).toBe(true)
    expect(short.length).toBeLessThanOrEqual(30)
  })

  it('cuts on whole segments, never in the middle of a name', () => {
    // "…ents/Ma Musique" is unreadable where "…/Ma Musique" keeps a
    // meaning. And it keeps as much of the tail as the budget allows, not
    // just the last segment: the immediate context helps get one's
    // bearings.
    expect(truncateStart('/a/bbbb/Documents/Ma Musique', 22)).toBe('…/Documents/Ma Musique')
    expect(truncateStart('/a/bbbb/Documents/Ma Musique', 16)).toBe('…/Ma Musique')
  })

  it('a single name longer than the budget is cut inside it, for lack of anything better', () => {
    // The fallback: a readable end beats a display that overflows.
    const short = truncateStart('/x/' + 'z'.repeat(80), 20)
    expect(short.length).toBeLessThanOrEqual(20)
    expect(short.startsWith('…')).toBe(true)
    expect(short.endsWith('z')).toBe(true)
  })

  it('keeps a share prefix when it fits', () => {
    // Form composed by the network wizard: the share must stay visible,
    // it is the landmark that was missing.
    expect(truncateStart('//192.168.1.15/music/Yann Tiersen', 52)).toBe(
      '//192.168.1.15/music/Yann Tiersen',
    )
  })
})

describe('volumes and exploration normalization', () => {
  it('a payload without the newer fields does not break the page', () => {
    // During a deployment, the plugin can be older than the page: absent
    // must mean "nothing", never an `undefined` that, crossing a `v-for`,
    // would break the whole render instead of an empty section.
    const d = normalizeData({})
    expect(d.volumes).toEqual([])
    expect(d.canBrowseSmb).toBe(false)
    expect(d.mountError).toBeNull()
    expect(d.explore.open).toBe(false)
    expect(d.explore.kind).toBeNull()
    expect(d.explore.dirs).toEqual([])
    expect(d.explore.shares).toEqual([])
    expect(d.explore.audioCount).toBe(0)
  })

  it('the volumes, the capability and the exploration read back', () => {
    const d = normalizeData({
      volumes: [{ path: '/media/usb', fstype: 'vfat' }],
      can_browse_smb: true,
      mount_error: 'Interactive authentication required.',
      explore: {
        open: true,
        kind: 'smb',
        host: 'nas',
        share: 'musique',
        path: 'Albums',
        shares: ['musique', 'photo'],
        dirs: ['Jazz'],
        audio_count: 12,
        busy: false,
        error: null,
      },
    })
    expect(d.volumes[0]).toEqual({ path: '/media/usb', fstype: 'vfat' })
    expect(d.canBrowseSmb).toBe(true)
    expect(d.mountError).toBe('Interactive authentication required.')
    expect(d.explore.kind).toBe('smb')
    expect(d.explore.audioCount).toBe(12)
    expect(d.explore.dirs).toEqual(['Jazz'])
    expect(d.explore.shares).toEqual(['musique', 'photo'])
  })

  it('an unknown wizard kind falls back to "none"', () => {
    // The kind drives whether the dialog displays. An unexpected value must
    // leave it closed rather than opening a half-composed panel.
    expect(normalizeData({ explore: { kind: 'ftp' } }).explore.kind).toBeNull()
  })

  it('an empty error means "nothing to report", not an error with no text', () => {
    // The plugin renders `null` when everything is fine; an empty string
    // says the same thing. Reading them differently would display a silent
    // error banner.
    expect(normalizeData({ explore: { error: '' } }).explore.error).toBeNull()
    expect(normalizeData({ mount_error: '' }).mountError).toBeNull()
  })
})
