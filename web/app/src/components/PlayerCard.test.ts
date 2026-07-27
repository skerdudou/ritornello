import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import PlayerCard from './PlayerCard.vue'
import type { PlayerPayload } from '../types'

/** Faux `EventSource` : jsdom n'en fournit pas. */
class FauxEventSource {
  static derniere: FauxEventSource | null = null
  onmessage: ((e: MessageEvent) => void) | null = null
  constructor(public url: string) {
    FauxEventSource.derniere = this
  }
  close() {}
  pousse(etat: Partial<PlayerPayload>) {
    const complet: PlayerPayload = {
      source: 'radio',
      volume: 60,
      muted: false,
      standby: false,
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
async function monteAvec(etat: Partial<PlayerPayload> | null) {
  vi.stubGlobal('EventSource', FauxEventSource)
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  const w = mount(PlayerCard)
  if (etat) {
    FauxEventSource.derniere!.pousse(etat)
    await w.vm.$nextTick()
  }
  return w
}

describe('PlayerCard', () => {
  it('affiche source et volume des la premiere trame', async () => {
    const w = await monteAvec({ source: 'cd', volume: 45 })
    expect(w.find('[data-source]').text()).toBe('cd')
    expect(w.find('[data-volume]').text()).toBe('45 %')
  })

  it('signale le muet et la veille', async () => {
    const w = await monteAvec({ muted: true, standby: true })
    expect(w.find('[data-muted]').exists()).toBe(true)
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('n affiche ni muet ni veille quand ils sont inactifs', async () => {
    const w = await monteAvec({ muted: false, standby: false })
    expect(w.find('[data-muted]').exists()).toBe(false)
    expect(w.find('[data-standby]').exists()).toBe(false)
  })

  it('suit les changements de volume sans rechargement', async () => {
    // Le volume peut changer depuis la telecommande infrarouge ou un autre
    // onglet : c'est tout l'objet du flux pousse.
    const w = await monteAvec({ volume: 60 })
    expect(w.find('[data-volume]').text()).toBe('60 %')
    FauxEventSource.derniere!.pousse({ volume: 65 })
    await w.vm.$nextTick()
    expect(w.find('[data-volume]').text()).toBe('65 %')
  })

  it('n affiche pas de bloc morceau tant que rien n est connu', async () => {
    // La plupart des stations francaises n'annoncent rien : un bloc « En
    // ecoute » vide ferait croire a une panne. L'encart du lecteur, lui, reste.
    const w = await monteAvec(null)
    expect(w.find('[data-player]').exists()).toBe(true)
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('n affiche pas de bloc morceau pour une duree seule', async () => {
    const w = await monteAvec({ duration_s: 214 })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('ajoute artiste, titre, album, duree et origine quand ils arrivent', async () => {
    const w = await monteAvec({
      artist: 'Miles Davis',
      title: 'So What',
      album: 'Kind of Blue',
      duration_s: 545,
      origin: 'musicbrainz',
    })
    expect(w.find('[data-now-playing]').exists()).toBe(true)
    expect(w.find('[data-titre]').text()).toBe('So What')
    expect(w.find('[data-artiste]').text()).toBe('Miles Davis')
    expect(w.find('[data-album]').text()).toBe('Kind of Blue')
    expect(w.find('[data-duree]').text()).toBe('9:05')
    expect(w.find('[data-origin]').text()).toBe('musicbrainz')
  })

  it('affiche un titre seul, tel que le donne l en-tete ICY', async () => {
    // L'ICY livre une chaine unique, non decoupee : elle arrive dans `title`.
    // Les webradios OUI FM l'emettent meme dans l'ordre « Titre - ARTISTE ».
    const w = await monteAvec({ title: 'Made Up - TAHITI 80', origin: 'icy' })
    expect(w.find('[data-titre]').text()).toBe('Made Up - TAHITI 80')
    expect(w.find('[data-artiste]').exists()).toBe(false)
    expect(w.find('[data-origin]').text()).toBe('icy')
  })

  it('affiche l artiste seul quand le titre manque', async () => {
    // Decision du proprietaire : toute information disponible est affichee.
    const w = await monteAvec({ artist: 'Téléphone', origin: 'ouifm-metas' })
    expect(w.find('[data-artiste]').text()).toBe('Téléphone')
    expect(w.find('[data-titre]').exists()).toBe(false)
  })

  it('retire le bloc morceau quand la lecture s arrete', async () => {
    // Changement d'identite ou arret : le coeur diffuse un etat sans morceau,
    // et l'ancien titre ne doit pas rester a l'ecran — mais l'encart du lecteur
    // reste, lui, avec la source et le volume.
    const w = await monteAvec({ title: 'premier' })
    FauxEventSource.derniere!.pousse({})
    await w.vm.$nextTick()
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.find('[data-player]').exists()).toBe(true)
  })
})
