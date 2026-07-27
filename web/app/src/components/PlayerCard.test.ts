import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import PlayerCard from './PlayerCard.vue'
import type { PlayerPayload } from '../types'

/**
 * Etat complet a partir d'un fragment : le composant recoit l'etat en prop —
 * c'est HomeView qui tient l'unique connexion SSE de la page (le flux reel
 * est couvre par les tests de HomeView et le parcours e2e).
 */
function complet(etat: Partial<PlayerPayload>): PlayerPayload {
  return {
    source: 'radio',
    volume: 60,
    muted: false,
    standby: false,
    preset: null,
    artist: null,
    title: null,
    album: null,
    duration_s: null,
    origin: null,
    ...etat,
  }
}

function monteAvec(etat: Partial<PlayerPayload> | null) {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(PlayerCard, { props: { etat: etat ? complet(etat) : null } })
}

describe('PlayerCard', () => {
  it('affiche source et volume des la premiere trame', () => {
    const w = monteAvec({ source: 'cd', volume: 45 })
    expect(w.find('[data-source]').text()).toBe('cd')
    expect(w.find('[data-volume]').text()).toBe('45 %')
  })

  it('signale le muet et la veille', () => {
    const w = monteAvec({ muted: true, standby: true })
    expect(w.find('[data-muted]').exists()).toBe(true)
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('n affiche ni muet ni veille quand ils sont inactifs', () => {
    const w = monteAvec({ muted: false, standby: false })
    expect(w.find('[data-muted]').exists()).toBe(false)
    expect(w.find('[data-standby]').exists()).toBe(false)
  })

  it('suit les changements de volume sans rechargement', async () => {
    // Le volume peut changer depuis la telecommande infrarouge ou un autre
    // onglet : c'est tout l'objet du flux pousse, relaye ici par la prop.
    const w = monteAvec({ volume: 60 })
    expect(w.find('[data-volume]').text()).toBe('60 %')
    await w.setProps({ etat: complet({ volume: 65 }) })
    expect(w.find('[data-volume]').text()).toBe('65 %')
  })

  it('n affiche pas de bloc morceau tant que rien n est connu', () => {
    // La plupart des stations francaises n'annoncent rien : un bloc « En
    // ecoute » vide ferait croire a une panne. L'encart du lecteur, lui, reste.
    const w = monteAvec(null)
    expect(w.find('[data-player]').exists()).toBe(true)
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('n affiche pas de bloc morceau pour une duree seule', () => {
    const w = monteAvec({ duration_s: 214 })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('ajoute artiste, titre, album, duree et origine quand ils arrivent', () => {
    const w = monteAvec({
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

  it('affiche un titre seul, tel que le donne l en-tete ICY', () => {
    // L'ICY livre une chaine unique, non decoupee : elle arrive dans `title`.
    // Les webradios OUI FM l'emettent meme dans l'ordre « Titre - ARTISTE ».
    const w = monteAvec({ title: 'Made Up - TAHITI 80', origin: 'icy' })
    expect(w.find('[data-titre]').text()).toBe('Made Up - TAHITI 80')
    expect(w.find('[data-artiste]').exists()).toBe(false)
    expect(w.find('[data-origin]').text()).toBe('icy')
  })

  it('affiche l artiste seul quand le titre manque', () => {
    // Decision du proprietaire : toute information disponible est affichee.
    const w = monteAvec({ artist: 'Téléphone', origin: 'ouifm-metas' })
    expect(w.find('[data-artiste]').text()).toBe('Téléphone')
    expect(w.find('[data-titre]').exists()).toBe(false)
  })

  it('retire le bloc morceau quand la lecture s arrete', async () => {
    // Changement d'identite ou arret : le coeur diffuse un etat sans morceau,
    // et l'ancien titre ne doit pas rester a l'ecran — mais l'encart du lecteur
    // reste, lui, avec la source et le volume.
    const w = monteAvec({ title: 'premier' })
    await w.setProps({ etat: complet({}) })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.find('[data-player]').exists()).toBe(true)
  })
})
