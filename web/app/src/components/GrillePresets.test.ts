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
    // Une page couvre 10k+1..10k+10 : la présélection 11 est donc la première
    // de la page 1, qui va de 11 à 12 sur douze stations.
    const w = monte({ preset_count: 12, preset: 11 })
    expect(w.get('[data-preset-count]').text()).toContain('12')
    expect(w.get('[data-preset-fenetre]').text()).toBe('11–12')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('la première page tient dix tuiles, la seconde commence à 11', async () => {
    // **La demande du propriétaire** : des pages de dix qui commencent à 1, 11,
    // 21. La grille valait auparavant 1-9 puis 10-19, parce que la touche 0 de
    // la télécommande ne valait rien seule ; elle vaut dix depuis (voir
    // `Command::Select` côté cœur), et les deux doivent nommer les mêmes
    // groupes.
    const w = monte({ preset_count: 23 })
    expect(w.findAll('[data-preset-button]').map((b) => b.attributes('data-preset-button')))
      .toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    expect(w.get('[data-preset-fenetre]').text()).toBe('1–10')

    await w.get('[data-preset-next]').trigger('click')
    expect(w.get('[data-preset-fenetre]').text()).toBe('11–20')
    await w.get('[data-preset-next]').trigger('click')
    expect(w.get('[data-preset-fenetre]').text()).toBe('21–23')
    // Et c'est la dernière : 23 stations tiennent en trois pages.
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('la page suit la présélection qui joue, dixième compris', async () => {
    // Le piège de la nouvelle borne : 10 appartient à la page 0, pas à la 1.
    const w = monte({ preset_count: 23, preset: 10 })
    expect(w.get('[data-preset-fenetre]').text()).toBe('1–10')
    await w.setProps({ etat: etat({ preset_count: 23, preset: 11 }) })
    expect(w.get('[data-preset-fenetre]').text()).toBe('11–20')
  })

  it('un compte pile sur une dizaine ne fabrique pas de page vide', () => {
    // Vingt stations : deux pages, pas trois. Une troisième nommerait 21-30,
    // où il n'y a rien — c'est la meme borne que le rebouclage du `+10`.
    const w = monte({ preset_count: 20 })
    expect(w.get('[data-preset-fenetre]').text()).toBe('1–10')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeUndefined()
  })

  it('sans compte déclaré, 1-10 nus et pas de flèches', () => {
    const w = monte({}, () => null)
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-count]').exists()).toBe(false)
  })
})
