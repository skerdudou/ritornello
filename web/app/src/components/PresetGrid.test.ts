import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import PresetGrid from './PresetGrid.vue'

const state = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, ...e,
})
const NAMES: Record<number, string> = { 1: 'FIP', 2: 'France Inter' }
const mounted = (e: Partial<PlayerPayload>, nameOf = (n: number) => NAMES[n] ?? null) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(PresetGrid, { props: { state: state(e), nameOf } })
}

describe('PresetGrid', () => {
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

  it('announces the count and the displayed window', () => {
    // A page covers 10k+1..10k+10: preset 11 is therefore the first of page 1,
    // which spans 11 to 12 on twelve stations.
    const w = mounted({ preset_count: 12, preset: 11 })
    expect(w.get('[data-preset-count]').text()).toContain('12')
    expect(w.get('[data-preset-window]').text()).toBe('11–12')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('the first page holds ten tiles, the second starts at 11', async () => {
    // **The owner's request**: pages of ten starting at 1, 11, 21. The grid
    // used to be 1-9 then 10-19, because key 0 on the remote was worth nothing
    // on its own; it has been worth ten since (see `Command::Select` core
    // side), and both must name the same groups.
    const w = mounted({ preset_count: 23 })
    expect(w.findAll('[data-preset-button]').map((b) => b.attributes('data-preset-button')))
      .toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    expect(w.get('[data-preset-window]').text()).toBe('1–10')

    await w.get('[data-preset-next]').trigger('click')
    expect(w.get('[data-preset-window]').text()).toBe('11–20')
    await w.get('[data-preset-next]').trigger('click')
    expect(w.get('[data-preset-window]').text()).toBe('21–23')
    // And it is the last one: 23 stations fit in three pages.
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('the page follows the playing preset, tenth included', async () => {
    // The trap of the new bound: 10 belongs to page 0, not to page 1.
    const w = mounted({ preset_count: 23, preset: 10 })
    expect(w.get('[data-preset-window]').text()).toBe('1–10')
    await w.setProps({ state: state({ preset_count: 23, preset: 11 }) })
    expect(w.get('[data-preset-window]').text()).toBe('11–20')
  })

  it('a count landing exactly on a decade does not fabricate an empty page', () => {
    // Twenty stations: two pages, not three. A third would name 21-30, where
    // there is nothing — same bound as the wrap-around of the `+10`.
    const w = mounted({ preset_count: 20 })
    expect(w.get('[data-preset-window]').text()).toBe('1–10')
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
