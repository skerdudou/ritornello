import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import LyricsDialog from './LyricsDialog.vue'
import { MANUAL_SCROLL_HOLD_MS } from '../composables/lyrics'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { RenderedLyrics } from '../types'

// Shapes as `GET /api/lyrics/{key}` serves them: both keys always present.
const TIMED: RenderedLyrics = {
  synced: [
    { t_ms: 1_000, text: 'First line' },
    { t_ms: 4_000, text: 'Second line' },
    { t_ms: 8_000, text: 'Third line' },
  ],
  plain: null,
}
const OTHER: RenderedLyrics = { synced: null, plain: 'Another song\nwith two lines' }

let fetchMock: ReturnType<typeof vi.fn>
let scrollIntoView: ReturnType<typeof vi.fn>
let wrapper: VueWrapper | null = null

beforeEach(() => {
  fetchMock = vi.fn(async (url: string) => {
    if (url === '/api/lyrics/aaa') return new Response(JSON.stringify(TIMED), { status: 200 })
    if (url === '/api/lyrics/bbb') return new Response(JSON.stringify(OTHER), { status: 200 })
    if (url === '/api/i18n') {
      return new Response(JSON.stringify({ lyrics_origin: 'Lyrics: {par}' }), { status: 200 })
    }
    return new Response('{}', { status: 404 })
  })
  vi.stubGlobal('fetch', fetchMock)
  // jsdom has none: stubbed to observe that the dialog follows the song.
  scrollIntoView = vi.fn()
  Element.prototype.scrollIntoView = scrollIntoView as unknown as Element['scrollIntoView']
})

afterEach(() => {
  resetCatalog()
  wrapper?.unmount()
  wrapper = null
  document.body.innerHTML = ''
  vi.restoreAllMocks()
  delete (Element.prototype as Partial<Element>).scrollIntoView
})

function mountDialog(props: {
  href: string | null
  synced?: boolean
  positionS?: number | null
  origin?: string | null
}) {
  wrapper = mount(LyricsDialog, {
    props: { synced: true, positionS: null, origin: 'files', ...props },
    attachTo: document.body,
  })
  return wrapper
}

async function openDialog(w: VueWrapper) {
  await w.get('[data-lyrics-open]').trigger('click')
  await flushPromises()
}

const dialog = () => document.body.querySelector('[data-lyrics-dialog]')
const lyricsCalls = () =>
  fetchMock.mock.calls.filter(([url]) => String(url).startsWith('/api/lyrics/'))

describe('LyricsDialog', () => {
  it('loads the lyrics only when opened', async () => {
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 5 })
    await flushPromises()
    // A request per song for a dialog nobody opened would be pure waste.
    expect(lyricsCalls()).toHaveLength(0)
    expect(dialog()).toBeNull()

    await openDialog(w)
    expect(lyricsCalls()).toHaveLength(1)
    expect(lyricsCalls()[0]![0]).toBe('/api/lyrics/aaa')
    expect(dialog()?.textContent).toContain('Second line')
  })

  it('shows no button without an href', () => {
    const w = mountDialog({ href: null })
    expect(w.find('[data-lyrics-open]').exists()).toBe(false)
  })

  it('highlights the current line from the position', async () => {
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 5 })
    await openDialog(w)
    const active = () => dialog()?.querySelectorAll('[data-lyrics-active]')
    expect(active()).toHaveLength(1)
    expect(active()?.[0]?.textContent?.trim()).toBe('Second line')
    expect(scrollIntoView).toHaveBeenLastCalledWith({ block: 'center', behavior: 'smooth' })

    // The song moves on: so does the highlight, and the scroll with it.
    scrollIntoView.mockClear()
    await w.setProps({ positionS: 9 })
    await flushPromises()
    expect(active()?.[0]?.textContent?.trim()).toBe('Third line')
    expect(scrollIntoView).toHaveBeenCalledTimes(1)
  })

  it('leaves the scrolling alone for a while after a hand scroll', async () => {
    let now = 1_000_000
    vi.spyOn(Date, 'now').mockImplementation(() => now)
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 2 })
    await openDialog(w)
    // Past the window in which a `scroll` event is taken for our own.
    now += 2_000
    scrollIntoView.mockClear()

    dialog()!.querySelector('[data-lyrics-scroll]')!.dispatchEvent(new Event('wheel'))
    await w.setProps({ positionS: 5 })
    await flushPromises()
    // The highlight still moves; only the scroll holds.
    expect(dialog()?.querySelector('[data-lyrics-active]')?.textContent?.trim()).toBe('Second line')
    expect(scrollIntoView).not.toHaveBeenCalled()

    now += MANUAL_SCROLL_HOLD_MS + 1
    await w.setProps({ positionS: 9 })
    await flushPromises()
    expect(scrollIntoView).toHaveBeenCalledTimes(1)
  })

  it('does not take its own smooth scroll for a hand scroll', async () => {
    let now = 1_000_000
    vi.spyOn(Date, 'now').mockImplementation(() => now)
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 2 })
    await openDialog(w)
    expect(scrollIntoView).toHaveBeenCalledTimes(1)
    // The `scroll` the smooth animation fires right after.
    now += 100
    dialog()!.querySelector('[data-lyrics-scroll]')!.dispatchEvent(new Event('scroll'))
    await w.setProps({ positionS: 5 })
    await flushPromises()
    expect(scrollIntoView).toHaveBeenCalledTimes(2)
  })

  it('shows the timed lines without highlight when the position is unknown', async () => {
    // A radio no plugin tracks: timed lines, no position.
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: null })
    await openDialog(w)
    expect(dialog()?.querySelector('[data-lyrics-active]')).toBeNull()
    expect(dialog()?.querySelector('[data-lyrics-text]')).toBeNull()
    const items = [...(dialog()?.querySelectorAll('[data-lyrics-lines] li') ?? [])]
    expect(items.map((li) => li.textContent?.trim())).toEqual(['First line', 'Second line', 'Third line'])
    expect(scrollIntoView).not.toHaveBeenCalled()
  })

  it('keeps the same list when the position is lost, only the highlight goes', async () => {
    // A Stop on a file: the position becomes unknown under the open dialog.
    // Swapping the list for a paragraph would lose the listener's place.
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 5 })
    await openDialog(w)
    const list = dialog()?.querySelector('[data-lyrics-lines]')
    expect(list).not.toBeNull()
    expect(dialog()?.querySelector('[data-lyrics-active]')?.textContent?.trim()).toBe('Second line')

    await w.setProps({ positionS: null })
    await flushPromises()
    expect(dialog()?.querySelector('[data-lyrics-lines]')).toBe(list)
    expect(dialog()?.querySelector('[data-lyrics-active]')).toBeNull()
    expect(dialog()?.querySelector('[data-lyrics-text]')).toBeNull()
    expect(list?.querySelectorAll('li')).toHaveLength(3)
  })

  it('shows the whole text when the lyrics are not timed, even with a position', async () => {
    const w = mountDialog({ href: '/api/lyrics/bbb', synced: false, positionS: 30 })
    await openDialog(w)
    expect(dialog()?.querySelector('[data-lyrics-active]')).toBeNull()
    expect(dialog()?.querySelector('[data-lyrics-text]')?.textContent).toBe(
      'Another song\nwith two lines',
    )
  })

  it('says there are no lyrics when they vanish while open, and stays open', async () => {
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 5 })
    await openDialog(w)
    expect(dialog()?.textContent).toContain('Second line')

    // The song changes under the open dialog, to one without lyrics.
    await w.setProps({ href: null, positionS: null })
    await flushPromises()
    expect(dialog()).not.toBeNull()
    expect(dialog()?.querySelector('[data-lyrics-none]')?.textContent).toBe('lyrics_none')
    expect(dialog()?.textContent).not.toContain('Second line')
    expect(w.find('[data-lyrics-open]').exists()).toBe(false)
  })

  it('follows a new href while open', async () => {
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 5 })
    await openDialog(w)
    await w.setProps({ href: '/api/lyrics/bbb', synced: false })
    await flushPromises()
    expect(lyricsCalls().map(([url]) => url)).toEqual(['/api/lyrics/aaa', '/api/lyrics/bbb'])
    expect(dialog()?.textContent).toContain('with two lines')
    expect(dialog()?.textContent).not.toContain('Second line')
  })

  it('names who provided the lyrics', async () => {
    await useCatalog().reload()
    const w = mountDialog({ href: '/api/lyrics/aaa', positionS: 5, origin: 'lrclib' })
    await openDialog(w)
    expect(dialog()?.querySelector('[data-lyrics-origin]')?.textContent?.trim()).toBe('Lyrics: lrclib')
  })

  it('says there are no lyrics when the key is gone', async () => {
    const w = mountDialog({ href: '/api/lyrics/evicted', positionS: 5 })
    await openDialog(w)
    expect(dialog()?.querySelector('[data-lyrics-none]')).not.toBeNull()
  })
})
