import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import NowPlaying from './NowPlaying.vue'
import type { NowPlayingPayload } from '../types'

/** Faux `EventSource` : jsdom n'en fournit pas. */
class FauxEventSource {
  static derniere: FauxEventSource | null = null
  onmessage: ((e: MessageEvent) => void) | null = null
  constructor(public url: string) {
    FauxEventSource.derniere = this
  }
  close() {}
  pousse(etat: Partial<NowPlayingPayload>) {
    const complet: NowPlayingPayload = {
      source: 'radio',
      artist: null,
      title: null,
      album: null,
      duration_s: null,
      origin: null,
      ...etat,
    }
    this.onmessage?.({ data: JSON.stringify(complet) } as MessageEvent)
  }
}

/** Monte le composant et pousse un etat, en rendant la vue a jour. */
async function monteAvec(etat: Partial<NowPlayingPayload> | null) {
  vi.stubGlobal('EventSource', FauxEventSource)
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  const w = mount(NowPlaying)
  if (etat) {
    FauxEventSource.derniere!.pousse(etat)
    await w.vm.$nextTick()
  }
  return w
}

describe('NowPlaying', () => {
  it('n affiche rien tant que rien n est connu', async () => {
    // La plupart des stations francaises n'annoncent rien : un cadre vide
    // portant « Morceau en cours » ferait croire a une panne.
    const w = await monteAvec(null)
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('n affiche rien pour un etat sans texte, meme avec une duree', async () => {
    const w = await monteAvec({ duration_s: 214 })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('affiche artiste, titre, album, duree et origine', async () => {
    const w = await monteAvec({
      artist: 'Miles Davis',
      title: 'So What',
      album: 'Kind of Blue',
      duration_s: 545,
      origin: 'musicbrainz',
    })
    expect(w.find('[data-titre]').text()).toBe('So What')
    expect(w.find('[data-artiste]').text()).toBe('Miles Davis')
    expect(w.find('[data-album]').text()).toBe('Kind of Blue')
    expect(w.find('[data-duree]').text()).toBe('9:05')
    expect(w.find('[data-origin]').text()).toBe('musicbrainz')
  })

  it('affiche un titre seul, tel que le donne l en-tete ICY', async () => {
    // L'ICY livre une chaine unique, non decoupee : elle arrive dans `title`.
    const w = await monteAvec({ title: 'Mandrillus Sphynx - Bikwix', origin: 'icy' })
    expect(w.find('[data-now-playing]').exists()).toBe(true)
    expect(w.find('[data-titre]').text()).toBe('Mandrillus Sphynx - Bikwix')
    expect(w.find('[data-artiste]').exists()).toBe(false)
    expect(w.find('[data-album]').exists()).toBe(false)
    expect(w.find('[data-origin]').text()).toBe('icy')
  })

  it('affiche l artiste seul quand le titre manque', async () => {
    // Decision du proprietaire : toute information disponible est affichee.
    const w = await monteAvec({ artist: 'Téléphone', origin: 'ouifm-metas' })
    expect(w.find('[data-artiste]').text()).toBe('Téléphone')
    expect(w.find('[data-titre]').exists()).toBe(false)
  })

  it('se met a jour au morceau suivant', async () => {
    const w = await monteAvec({ title: 'premier' })
    expect(w.find('[data-titre]').text()).toBe('premier')
    FauxEventSource.derniere!.pousse({ title: 'second' })
    await w.vm.$nextTick()
    expect(w.find('[data-titre]').text()).toBe('second')
  })

  it('disparait quand le morceau s arrete', async () => {
    // Changement d'identite : le coeur diffuse un etat vide, et l'ancien titre
    // ne doit pas rester a l'ecran.
    const w = await monteAvec({ title: 'premier' })
    FauxEventSource.derniere!.pousse({})
    await w.vm.$nextTick()
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })
})
