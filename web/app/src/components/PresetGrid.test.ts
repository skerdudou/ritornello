import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'
import PresetGrid from './PresetGrid.vue'
import { LAYOUT_KEY, revealScrollTop } from './presetLayout'

const state = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, has_finite_list: false, random: false, ...e,
})
const NAMES: Record<number, string> = { 1: 'FIP', 2: 'France Inter' }
// The shell's catalog is module-level state: load the two keys the card
// renders with params, as App.vue does, so the test reads the real text.
const CATALOG = {
  presets_label: 'Presets', presets_window: '{first}–{last} of {total}',
  presets_show_list: 'Show every preset in one list', presets_show_pages: 'Show the presets in pages of ten',
}
beforeEach(async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(CATALOG), { status: 200 })))
  await useCatalog().reload()
})
afterEach(() => resetCatalog())

const mounted = (e: Partial<PlayerPayload>, nameOf = (n: number) => NAMES[n] ?? null, listedCount = 0) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(PresetGrid, { props: { state: state(e), nameOf, listedCount } })
}

describe('PresetGrid', () => {
  it('in standby, shows the presets the catalog lists for the source', () => {
    // Standby forgets the announced count on the core side; the catalog
    // still knows the stations.
    const w = mounted({ standby: true, preset_count: null }, undefined, 2)
    expect(w.findAll('[data-preset-button]')).toHaveLength(2)
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    for (const b of w.findAll('[data-preset-button]')) expect(b.attributes('disabled')).toBeDefined()
  })

  it('in standby, a source the catalog lists nothing for shows an empty grid', () => {
    // A cd: no fixed 1-10 grid of ten dead tiles.
    const w = mounted({ standby: true, preset_count: null }, undefined, 0)
    expect(w.findAll('[data-preset-button]')).toHaveLength(0)
  })

  it('awake, the announced count rules, not the catalog', () => {
    const w = mounted({ standby: false, preset_count: 3 }, undefined, 2)
    expect(w.findAll('[data-preset-button]')).toHaveLength(3)
  })

  it('names the tiles the source names, bare number otherwise', () => {
    const w = mounted({ preset_count: 3, preset: 1 })
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    expect(w.get('[data-preset-button="2"] [data-preset-name]').text()).toBe('France Inter')
    expect(w.find('[data-preset-button="3"] [data-preset-name]').exists()).toBe(false)
    expect(w.get('[data-preset-button="3"]').text()).toBe('3')
  })

  it('highlights the playing tile', () => {
    const w = mounted({ preset_count: 3, preset: 2 })
    expect(w.get('[data-preset-button="2"]').attributes('aria-current')).toBe('true')
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
  })

  it('emits the chosen number', async () => {
    const w = mounted({ preset_count: 3 })
    await w.get('[data-preset-button="3"]').trigger('click')
    expect(w.emitted('choose')).toEqual([[3]])
  })

  it('shows no lone count, and the position in the header when there are pages', () => {
    const w = mounted({ preset_count: 12, preset: 11 })
    expect(w.find('[data-preset-count]').exists()).toBe(false)
    const header = w.get('[data-slot="card-header"]')
    expect(header.text()).toContain('Presets')
    expect(header.find('[data-preset-prev]').exists()).toBe(true)
    expect(header.find('[data-preset-next]').exists()).toBe(true)
    expect(header.get('[data-preset-window]').text()).toBe('11–12 of 12')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('without pages, the header holds the title alone', () => {
    const w = mounted({ preset_count: 3 })
    const header = w.get('[data-slot="card-header"]')
    expect(header.find('[data-preset-window]').exists()).toBe(false)
    expect(header.find('[data-preset-prev]').exists()).toBe(false)
  })

  it('leaving standby, the announced count rules again', async () => {
    const w = mounted({ standby: true, preset_count: null }, undefined, 2)
    expect(w.findAll('[data-preset-button]')).toHaveLength(2)
    await w.setProps({ state: state({ standby: false, preset_count: 23, preset: 15 }) })
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.get('[data-preset-window]').text()).toBe('11–20 of 23')
  })

  it('the first page holds ten tiles, the second starts at 11', async () => {
    // **The owner's request**: pages of ten starting at 1, 11, 21. The grid
    // used to be 1-9 then 10-19, because key 0 on the remote was worth nothing
    // on its own; it has been worth ten since (see `Command::Select` core
    // side), and both must name the same groups.
    const w = mounted({ preset_count: 23 })
    expect(w.findAll('[data-preset-button]').map((b) => b.attributes('data-preset-button')))
      .toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    expect(w.get('[data-preset-window]').text()).toBe('1–10 of 23')

    await w.get('[data-preset-next]').trigger('click')
    expect(w.get('[data-preset-window]').text()).toBe('11–20 of 23')
    await w.get('[data-preset-next]').trigger('click')
    expect(w.get('[data-preset-window]').text()).toBe('21–23 of 23')
    // And it is the last one: 23 stations fit in three pages.
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('the page follows the playing preset, tenth included', async () => {
    // The trap of the new bound: 10 belongs to page 0, not to page 1.
    const w = mounted({ preset_count: 23, preset: 10 })
    expect(w.get('[data-preset-window]').text()).toBe('1–10 of 23')
    await w.setProps({ state: state({ preset_count: 23, preset: 11 }) })
    expect(w.get('[data-preset-window]').text()).toBe('11–20 of 23')
  })

  it('a count landing exactly on a decade does not fabricate an empty page', () => {
    // Twenty stations: two pages, not three. A third would name 21-30, where
    // there is nothing — same bound as the wrap-around of the `+10`.
    const w = mounted({ preset_count: 20 })
    expect(w.get('[data-preset-window]').text()).toBe('1–10 of 20')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeUndefined()
  })

  it('without a declared count, bare 1-10 and no arrows', () => {
    const w = mounted({}, () => null)
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-count]').exists()).toBe(false)
  })
})

describe('layout', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    localStorage.clear()
  })

  const toggle = (w: ReturnType<typeof mounted>) => w.get('[data-preset-layout-toggle]')
  const layoutOf = (w: ReturnType<typeof mounted>) => w.get('[data-preset-grid]').attributes('data-preset-layout')

  it('shows the layout toggle even when everything fits on one page', () => {
    const w = mounted({ preset_count: 10 })
    expect(w.find('[data-preset-layout-toggle]').exists()).toBe(true)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-window]').exists()).toBe(false)
  })

  it('switches to a list of every preset, without arrows or window, and back', async () => {
    const w = mounted({ preset_count: 23 })
    await toggle(w).trigger('click')
    expect(w.find('[data-preset-list]').exists()).toBe(true)
    expect(w.findAll('[data-preset-button]')).toHaveLength(23)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
    expect(w.find('[data-preset-window]').exists()).toBe(false)
    expect(layoutOf(w)).toBe('list')

    await toggle(w).trigger('click')
    expect(w.find('[data-preset-list]').exists()).toBe(false)
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-prev]').exists()).toBe(true)
    expect(w.find('[data-preset-next]').exists()).toBe(true)
    expect(layoutOf(w)).toBe('pages')
  })

  it('remembers the layout for the next mount', async () => {
    const w = mounted({ preset_count: 23 })
    await toggle(w).trigger('click')
    w.unmount()
    const again = mounted({ preset_count: 23 })
    expect(layoutOf(again)).toBe('list')
    expect(localStorage.getItem(LAYOUT_KEY)).toBe('list')
  })

  it('starts in pages when storage is unavailable, and the toggle still works', async () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('denied') })
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('denied') })
    const w = mounted({ preset_count: 23 })
    expect(layoutOf(w)).toBe('pages')
    await toggle(w).trigger('click')
    expect(layoutOf(w)).toBe('list')
    expect(w.find('[data-preset-list]').exists()).toBe(true)
  })

  it('a stored list with ten presets or fewer shows them as a list', () => {
    localStorage.setItem(LAYOUT_KEY, 'list')
    const w = mounted({ preset_count: 8 })
    expect(layoutOf(w)).toBe('list')
    expect(w.get('[data-preset-list]').findAll('[data-preset-button]')).toHaveLength(8)
  })

  it('in list, the toggle offers the pages and the tile emits its number', async () => {
    localStorage.setItem(LAYOUT_KEY, 'list')
    const w = mounted({ preset_count: 23 })
    expect(toggle(w).attributes('aria-label')).toBe('Show the presets in pages of ten')
    await w.get('[data-preset-button="17"]').trigger('click')
    expect(w.emitted('choose')).toEqual([[17]])
  })

  it('in pages, the toggle offers the list', () => {
    const w = mounted({ preset_count: 23 })
    expect(toggle(w).attributes('aria-label')).toBe('Show every preset in one list')
  })

  it('back to pages, shows the page of the active preset', async () => {
    const w = mounted({ preset_count: 23, preset: 17 })
    await toggle(w).trigger('click')
    await w.setProps({ state: state({ preset_count: 23, preset: 4 }) })
    await toggle(w).trigger('click')
    expect(w.get('[data-preset-window]').text()).toBe('1–10 of 23')
  })

  it('in list, scrolls the frame, not the page, to the active preset', async () => {
    // jsdom has no layout: give the frame and row 17 the geometry the
    // computation reads, and back scrollTop with a map so the write is visible.
    const proto = HTMLElement.prototype as unknown as Record<string, unknown>
    // jsdom defines offsetTop/offsetHeight on this very prototype: keep the
    // originals to put them back, rather than deleting them.
    const own = (k: string) => Object.getOwnPropertyDescriptor(proto, k)
    const originals = new Map(['clientHeight', 'scrollHeight', 'offsetTop', 'offsetHeight', 'scrollTop', 'scrollIntoView'].map((k) => [k, own(k)]))
    const scrollIntoView = vi.fn()
    proto.scrollIntoView = scrollIntoView
    const tops = new WeakMap<object, number>()
    const isFrame = (el: HTMLElement) => el.hasAttribute('data-preset-list')
    const isRow = (el: HTMLElement) => el.getAttribute('data-preset-button') === '17'
    Object.defineProperty(proto, 'clientHeight', { configurable: true, get(this: HTMLElement) { return isFrame(this) ? 200 : 0 } })
    Object.defineProperty(proto, 'scrollHeight', { configurable: true, get(this: HTMLElement) { return isFrame(this) ? 1300 : 0 } })
    Object.defineProperty(proto, 'offsetTop', { configurable: true, get(this: HTMLElement) { return isRow(this) ? 900 : 0 } })
    Object.defineProperty(proto, 'offsetHeight', { configurable: true, get(this: HTMLElement) { return isRow(this) ? 48 : 0 } })
    Object.defineProperty(proto, 'scrollTop', {
      configurable: true,
      get(this: HTMLElement) { return tops.get(this) ?? 0 },
      set(this: HTMLElement, v: number) { tops.set(this, v) },
    })
    try {
      const w = mounted({ preset_count: 23, preset: 17 })
      await toggle(w).trigger('click')
      await w.vm.$nextTick()
      const frame = w.get('[data-preset-list]').element as HTMLElement
      expect(frame.scrollTop).toBe(revealScrollTop(0, 200, 900, 48, 1300))
      expect(frame.scrollTop).toBe(900 + 24 - 100)
      expect(scrollIntoView).not.toHaveBeenCalled()
    } finally {
      for (const [k, d] of originals) {
        if (d) Object.defineProperty(proto, k, d)
        else delete proto[k]
      }
    }
  })
})

describe('swipe', () => {
  afterEach(() => {
    localStorage.clear()
  })

  // jsdom does not synthesize the click that ends a touch gesture: dispatch it.
  async function swipe(el: Element, from: [number, number], to: [number, number], pointerType = 'touch') {
    el.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerId: 1, pointerType, clientX: from[0], clientY: from[1] }))
    el.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerId: 1, pointerType, clientX: to[0], clientY: to[1] }))
    el.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    await nextTick()
  }
  const windowOf = (w: ReturnType<typeof mounted>) => w.get('[data-preset-window]').text()

  it('a left swipe shows the next page, a right swipe the previous one', async () => {
    const w = mounted({ preset_count: 23 })
    const grid = w.get('[data-preset-grid]').element
    await swipe(grid, [200, 100], [100, 110])
    expect(windowOf(w)).toBe('11–20 of 23')
    await swipe(grid, [200, 100], [100, 110])
    expect(windowOf(w)).toBe('21–23 of 23')
    await swipe(grid, [200, 100], [100, 110])
    expect(windowOf(w)).toBe('21–23 of 23')
    await swipe(grid, [100, 100], [200, 100])
    expect(windowOf(w)).toBe('11–20 of 23')
  })

  it('a swipe starting on a tile turns the page without choosing the tile', async () => {
    const w = mounted({ preset_count: 23 })
    await swipe(w.get('[data-preset-button="3"]').element, [200, 100], [100, 110])
    expect(windowOf(w)).toBe('11–20 of 23')
    expect(w.emitted('choose')).toBeUndefined()
  })

  it('a plain tap on a tile still chooses it', async () => {
    const w = mounted({ preset_count: 23 })
    await swipe(w.get('[data-preset-button="3"]').element, [50, 50], [52, 51])
    expect(w.emitted('choose')).toEqual([[3]])
    expect(windowOf(w)).toBe('1–10 of 23')
  })

  it('a mostly vertical drag does not turn the page', async () => {
    const w = mounted({ preset_count: 23 })
    await swipe(w.get('[data-preset-grid]').element, [200, 100], [150, 300])
    expect(windowOf(w)).toBe('1–10 of 23')
  })

  it('a cancelled gesture does not turn the page', async () => {
    const w = mounted({ preset_count: 23 })
    const grid = w.get('[data-preset-grid]').element
    grid.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerId: 1, pointerType: 'touch', clientX: 200, clientY: 100 }))
    grid.dispatchEvent(new PointerEvent('pointercancel', { bubbles: true, pointerId: 1, pointerType: 'touch' }))
    grid.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerId: 1, pointerType: 'touch', clientX: 100, clientY: 100 }))
    await nextTick()
    expect(windowOf(w)).toBe('1–10 of 23')
  })

  it('a mouse drag does not turn the page', async () => {
    const w = mounted({ preset_count: 23 })
    await swipe(w.get('[data-preset-grid]').element, [200, 100], [100, 110], 'mouse')
    expect(windowOf(w)).toBe('1–10 of 23')
  })

  it('in list mode a swipe does nothing', async () => {
    const w = mounted({ preset_count: 23 })
    await w.get('[data-preset-layout-toggle]').trigger('click')
    await swipe(w.get('[data-preset-grid]').element, [200, 100], [100, 110])
    expect(w.get('[data-preset-grid]').attributes('data-preset-layout')).toBe('list')
    expect(w.emitted('choose')).toBeUndefined()
    // Back to pages: still the first page, the swipe turned nothing behind the list.
    await w.get('[data-preset-layout-toggle]').trigger('click')
    expect(windowOf(w)).toBe('1–10 of 23')
  })

  it('the card lets the page scroll vertically in pages mode', async () => {
    const w = mounted({ preset_count: 23 })
    expect(w.get('[data-preset-grid]').attributes('style')).toContain('touch-action: pan-y')
    await w.get('[data-preset-layout-toggle]').trigger('click')
    expect(w.get('[data-preset-grid]').attributes('style') ?? '').not.toContain('touch-action')
  })
})
