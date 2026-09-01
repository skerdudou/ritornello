import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOG, server } from './harness'

/** A level as `scan::list_dir` returns it: **names**, not paths. */
interface Level {
  dirs: string[]
  files: string[]
  /** `.m3u`/`.m3u8` files: they load, they are not added. */
  playlists?: string[]
}

/**
 * Stand-in for a share: the plugin returns only **one** level per `browse`,
 * the one it is asked for, and it stores browse and search in the same place.
 * `query` is what tells them apart.
 */
function tree(
  levels: Record<string, Level>,
  findings: string[] = [],
  truncated = false,
  abort = false,
) {
  const s = server({ roots: [{ name: 'nas', kind: 'smb', host: 'h', share: 'musique' }] })
  s.onPut = (payload) => {
    const path = String(payload.path ?? '')
    if (payload.op === 'browse') {
      const n = levels[path] ?? { dirs: [], files: [] }
      s.data.browse = {
        root: 'nas',
        path: path,
        query: '',
        dirs: n.dirs,
        files: n.files,
        playlists: n.playlists ?? [],
        results: [],
      }
    }
    if (payload.op === 'search') {
      s.data.browse = {
        root: 'nas',
        path: path,
        query: String(payload.query ?? ''),
        dirs: [],
        files: [],
        results: findings,
        truncated: truncated,
        gave_up: abort,
      }
    }
  }
  return s
}

const LEVELS: Record<string, Level> = {
  '': { dirs: ['Albums'], files: ['jingle.mp3'] },
  Albums: { dirs: ['Jazz'], files: ['01.mp3'], playlists: ['tout.m3u'] },
  'Albums/Jazz': { dirs: [], files: ['Kind of Blue.flac'] },
}

async function mountTree(findings: string[] = [], truncated = false, abort = false) {
  const s = tree(LEVELS, findings, truncated, abort)
  const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, s }
}

/** Names displayed in the current level, folders as well as files. */
function names(w: ReturnType<typeof mount>): string[] {
  return w.findAll('[data-browse-name]').map((n) => n.text())
}

describe('file browser', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('opens only one level when the page loads', async () => {
    // Encoded regression: asking for the whole tree of a share holding several
    // tens of thousands of files would far exceed the core's 5 s cap — the page
    // would display nothing at all.
    const { w, s } = await mountTree()
    expect(s.putsOf('browse')).toEqual([{ op: 'browse', root: 'nas', path: '' }])
    expect(names(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('goes down into a folder and REPLACES the displayed level', async () => {
    // A browser, not a tree: this is what bounds the height of the list. The
    // sent path is recomposed — `Albums/Jazz` and not `Jazz`, which the plugin
    // would resolve against the root, hence elsewhere.
    const { w, s } = await mountTree()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(s.putsOf('browse').map((b) => b.path)).toEqual(['', 'Albums'])
    expect(names(w)).toEqual(['Jazz', 'tout.m3u', '01.mp3'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(s.putsOf('browse').map((b) => b.path)).toEqual(['', 'Albums', 'Albums/Jazz'])
    expect(names(w)).toEqual(['Kind of Blue.flac'])
  })

  it('goes up to the parent, and does not offer it at the top', async () => {
    const { w, s } = await mountTree()
    expect(w.find('[data-browse-up]').attributes('disabled')).toBeDefined()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-browse-up]').attributes('disabled')).toBeUndefined()
    await w.find('[data-browse-up]').trigger('click')
    await flushPromises()
    expect(s.putsOf('browse').map((b) => b.path)).toEqual(['', 'Albums', ''])
    expect(names(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('displays the open path, root included', async () => {
    // Without the root name, a relative path does not say where we are when
    // several sources are declared.
    const { w } = await mountTree()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-browse-path]').attributes('title')).toBe('nas/Albums')
  })

  it('adds the open folder, except at the top', async () => {
    // At the top the gesture already exists on the source's row (Sources pane):
    // two buttons for the same effect made one look for a difference that did
    // not exist.
    const { w, s } = await mountTree()
    expect(w.find('[data-add-current]').exists()).toBe(false)
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-add-current]').trigger('click')
    await flushPromises()
    expect(s.putsOf('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
  })

  it('adds a listed folder recursively, and a single file', async () => {
    const { w, s } = await mountTree()
    await w.find('[data-add-dir]').trigger('click')
    await flushPromises()
    expect(s.putsOf('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
    await w.find('[data-add-file]').trigger('click')
    await flushPromises()
    expect(s.putsOf('add_file')).toEqual([{ op: 'add_file', root: 'nas', path: 'jingle.mp3' }])
  })

  it('an m3u **loads**, it is not added', async () => {
    // The action is deliberately different from that of tracks: a playlist
    // replaces the current playlist. Confusing them would add a text file that
    // mpv would try to play.
    const { w, s } = await mountTree()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    const rows = w.findAll('[data-browse-row]')
    const m3uRow = rows.find((r) => r.find('[data-browse-name]').text() === 'tout.m3u')
    expect(m3uRow).toBeDefined()
    expect(m3uRow!.find('[data-add-file]').exists()).toBe(false)
    await m3uRow!.find('[data-load-m3u]').trigger('click')
    await flushPromises()
    expect(s.putsOf('load_m3u')).toEqual([
      { op: 'load_m3u', root: 'nas', path: 'Albums/tout.m3u' },
    ])
    expect(s.putsOf('add_file')).toEqual([])
  })

  it('searches in the open folder, without erasing the displayed level', async () => {
    // Both live in the same place on the plugin side: if the page read the
    // level from the answer, a search would empty the list before one's eyes.
    const { w, s } = await mountTree(['Albums/Jazz/miles.flac'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsOf('search')).toEqual([
      { op: 'search', root: 'nas', path: 'Albums', query: 'miles' },
    ])
    // The full path, not only the name: a search reports namesakes coming from
    // different folders, and nothing else tells them apart.
    expect(w.find('[data-search-row]').text()).toContain('Albums/Jazz/miles.flac')
    expect(names(w)).toContain('Jazz')
  })

  it('says which folder the search bears on', async () => {
    const { w } = await mountTree()
    expect(w.find('[data-search-scope]').text()).toContain('nas')
  })

  it('adds a search result by its path', async () => {
    const { w, s } = await mountTree(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    await w.find('[data-add-result]').trigger('click')
    await flushPromises()
    expect(s.putsOf('add_file')).toEqual([
      { op: 'add_file', root: 'nas', path: 'Albums/Jazz/miles.flac' },
    ])
  })

  it('flags a truncated search instead of presenting it as complete', async () => {
    // Encoded regression: `scan::search` caps at 200 results and says so via
    // `truncated`. Without this sentence, the user who does not see their file
    // concludes that it is not there.
    const { w } = await mountTree(['Albums/Jazz/miles.flac'], true)
    await w.find('[data-search-query]').setValue('a')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-truncated]').text()).toContain('narrow')
  })

  it('an abandoned search does not pass itself off as "no result"', async () => {
    // Review defect: the walk returns `Ok(true)` whether the cap reached is the
    // one on visits or the one on results, and "No result" — "this file does
    // not exist" — was displayed for a search that had simply given up before
    // reaching the wanted file.
    const { w } = await mountTree([], false, true)
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-no-results]').exists()).toBe(false)
    expect(w.find('[data-search-gave-up]').exists()).toBe(true)
  })

  it('going down after a search erases the results, not only the level', async () => {
    // Observed symptom: `search_scope` is a live caption (`computed` on the
    // open folder). Without erasing `results`/`query` on folder change, one
    // gets results for "miles" in Albums displayed under a caption that already
    // announces "Jazz", and a search field that still looks active although it
    // no longer matches anything.
    const { w } = await mountTree(['Albums/Jazz/miles.flac'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-results]').exists()).toBe(true)
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-results]').exists()).toBe(false)
    expect((w.find('[data-search-query]').element as HTMLInputElement).value).toBe('')
  })

  it('an empty search emits nothing', async () => {
    const { w, s } = await mountTree()
    await w.find('[data-search-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsOf('search')).toHaveLength(0)
  })

  it('does not mistake a search answer for a level', async () => {
    // Guard of the `query` marker: without it, the answer to a search bearing
    // on the open folder would fill the level with its results, that is with
    // nothing at all (`dirs` and `files` are empty there).
    const { w } = await mountTree(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(names(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('a refused browse does not make the folder pass for empty', async () => {
    // Storing an empty level after a refusal would make it pass for an empty
    // folder, and the user would have no way to retry without reloading the
    // page.
    const s = tree(LEVELS)
    s.refusal = 'could not read "Albums": the share may be unreachable'
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.find('[data-message]').text()).toBe(s.refusal)
    expect(w.find('[data-browse-empty]').exists()).toBe(false)
  })

  it('without a declared root, the pane says so instead of emitting a browse', async () => {
    const s = server()
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(s.putsOf('browse')).toHaveLength(0)
    // Same sentence as the Sources pane: two wordings for the same emptiness
    // would suggest two different causes.
    expect(w.find('[data-browse-pane]').text()).toContain('No source declared')
  })
})
