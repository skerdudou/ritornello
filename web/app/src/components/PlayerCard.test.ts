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
    preset_count: null,
    preset_name: null,
    status: null,
    overlay: null,
    artist: null,
    title: null,
    album: null,
    duration_s: null,
    origin: null,
    position_s: null,
    seekable: false,
    can_eject: false,
    ...etat,
  }
}

function monteAvec(etat: Partial<PlayerPayload> | null) {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(PlayerCard, { props: { etat: etat ? complet(etat) : null, pasDeplacement: 10 } })
}

describe('PlayerCard', () => {
  it('affiche source et volume des la premiere trame', () => {
    const w = monteAvec({ source: 'cd', volume: 45 })
    expect(w.find('[data-source]').text()).toBe('cd')
    expect(w.find('[data-volume]').text()).toBe('45 %')
  })

  it('affiche la présélection en cours quand la Source en déclare une', () => {
    const w = monteAvec({ preset: 4 })
    expect(w.find('[data-player-preset]').text()).toBe('4')
  })

  it('n affiche pas de ligne de présélection quand la Source n en déclare aucune', () => {
    // `null` couvre deux situations où il n'y a rien à numéroter — rien ne joue,
    // ou la Source ne numérote pas (cd sans disque, entrée auxiliaire) — et une
    // ligne vide y laisserait croire à une panne.
    const w = monteAvec({ preset: null })
    expect(w.find('[data-player-preset]').exists()).toBe(false)
  })

  it('affiche la présélection 0 plutôt que de la confondre avec une absence', () => {
    // Garde contre un `v-if` écrit sur la valeur elle-même : `0` est faux en
    // JavaScript mais reste une présélection déclarée.
    const w = monteAvec({ preset: 0 })
    expect(w.find('[data-player-preset]').text()).toBe('0')
  })

  it('ajoute le nom de la présélection quand la Source en déclare un', () => {
    const w = monteAvec({ preset: 4, preset_name: 'FIP' })
    expect(w.find('[data-player-preset]').text()).toBe('4')
    expect(w.find('[data-player-preset-name]').text()).toBe('FIP')
  })

  it('n affiche que le numéro quand la Source ne nomme rien', () => {
    // Cas du cd : une présélection déclarée (la piste), mais aucun nom — pas
    // de clé i18n générique du type « station » qui serait fausse ici.
    const w = monteAvec({ preset: 3, preset_name: null })
    expect(w.find('[data-player-preset]').text()).toBe('3')
    expect(w.find('[data-player-preset-name]').exists()).toBe(false)
  })

  it('affiche le statut déclaré par la source', () => {
    const w = monteAvec({ status: 'PAS DE DISQUE' })
    expect(w.find('[data-player-status]').text()).toBe('PAS DE DISQUE')
  })

  it('n affiche aucune ligne de statut quand il n y en a pas', () => {
    const w = monteAvec({ status: null })
    expect(w.find('[data-player-status]').exists()).toBe(false)
  })

  it('masque la ligne de statut en veille pour ne pas doubler le badge VEILLE', () => {
    // Le statut publié en veille est le même mot du même catalogue que le
    // badge "VEILLE" affiché juste au-dessus (voir M2, revue de branche) :
    // sans ce masquage, la carte montrerait "VEILLE" deux fois, la seconde
    // sans libellé contrairement à ses voisines ("Présélection :", "Volume :").
    const w = monteAvec({ status: 'VEILLE', standby: true })
    expect(w.find('[data-player-status]').exists()).toBe(false)
    expect(w.find('[data-standby]').exists()).toBe(true)
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

  it('montre la barre quand une position est connue', () => {
    const w = mount(PlayerCard, {
      props: {
        etat: complet({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        pasDeplacement: 10,
      },
    })
    expect(w.find('[data-barre]').exists()).toBe(true)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('n affiche pas la duree en en-tete quand la barre la porte deja', () => {
    // Defaut corrige : titre + position connus (le cas nominal d'un CD
    // reconnu) affichait "4:14" en en-tete ET "1:27 ... 4:14" dans la barre,
    // deux fois la meme information.
    const w = mount(PlayerCard, {
      props: {
        etat: complet({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        pasDeplacement: 10,
      },
    })
    expect(w.find('[data-duree]').exists()).toBe(false)
    expect(w.get('[data-duree-totale]').text()).toBe('4:14')
  })

  it('ne montre rien de la progression quand aucune position n est connue', () => {
    const w = mount(PlayerCard, {
      props: {
        etat: complet({ title: 'Bikwix', position_s: null, duration_s: 254 }),
        pasDeplacement: 10,
      },
    })
    expect(w.find('[data-position]').exists()).toBe(false)
  })

  // Sans titre ni artiste ni album, le bloc « en ecoute » est masque : la
  // progression, elle, doit rester visible. C'est le cas d'un fichier sans
  // etiquettes ou d'un disque que MusicBrainz ne reconnait pas, ou mpv
  // connait pourtant parfaitement la position.
  it('montre la progression meme sans aucune metadonnee', () => {
    const w = mount(PlayerCard, {
      props: {
        etat: complet({ position_s: 87, duration_s: 254, seekable: true }),
        pasDeplacement: 10,
      },
    })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.get('[data-position]').text()).toBe('1:27')
    expect(w.find('[data-barre]').exists()).toBe(true)
  })
})
