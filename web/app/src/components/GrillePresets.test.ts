import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import GrillePresets from './GrillePresets.vue'

const etat = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, ...e,
})
const NOMS: Record<number, string> = { 1: 'FIP', 2: 'France Inter' }
const monte = (e: Partial<PlayerPayload>, nomDe = (n: number) => NOMS[n] ?? null) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(GrillePresets, { props: { etat: etat(e), nomDe } })
}

describe('GrillePresets', () => {
  it('nomme les tuiles que la source nomme, numéro seul sinon', () => {
    const w = monte({ preset_count: 3, preset: 1 })
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    expect(w.get('[data-preset-button="2"] [data-preset-name]').text()).toBe('France Inter')
    expect(w.find('[data-preset-button="3"] [data-preset-name]').exists()).toBe(false)
    expect(w.get('[data-preset-button="3"]').text()).toBe('3')
  })

  it('met en évidence la tuile qui joue', () => {
    const w = monte({ preset_count: 3, preset: 2 })
    expect(w.get('[data-preset-button="2"]').attributes('aria-current')).toBe('true')
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
  })

  it('émet le numéro choisi', async () => {
    const w = monte({ preset_count: 3 })
    await w.get('[data-preset-button="3"]').trigger('click')
    expect(w.emitted('choisir')).toEqual([[3]])
  })

  it('annonce le compte et la fenêtre affichée', () => {
    const w = monte({ preset_count: 12, preset: 11 })
    expect(w.get('[data-preset-count]').text()).toContain('12')
    expect(w.get('[data-preset-fenetre]').text()).toBe('10–12')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('sans compte déclaré, 1-9 nus et pas de flèches', () => {
    const w = monte({}, () => null)
    expect(w.findAll('[data-preset-button]')).toHaveLength(9)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-count]').exists()).toBe(false)
  })
})
