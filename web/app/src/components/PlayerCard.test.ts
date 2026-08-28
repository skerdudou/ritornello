import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import PlayerCard from './PlayerCard.vue'
import type { PlayerPayload } from '../types'

// La color de marque : l'exception assumee a la regle « aucune color en
// dur » (decision du proprietaire, voir docs/interface.md § Player card).
// Verifier qu'elle est bien la, plateforme par plateforme, documente
// l'exception autant que ca ne la prouve.
const COULEUR_ICONE = {
  youtube: '#FF0000',
  deezer: '#A238FF',
  apple_music: '#FA243C',
} as const

// jsdom ne fournit step ResizeObserver ; reka-ui l'utilise pour mesurer la
// piste du curseur de ProgressBar, mounted ici des que `seekable` est vrai
// (voir web/kit/src/index.test.ts et ProgressBar.test.ts).
beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
})

/**
 * Etat complet a partir d'un fragment : le composant recoit l'state en prop —
 * c'est HomeView qui tient l'unique connexion SSE de la page (le flux reel
 * est couvre par les tests de HomeView et le journey e2e).
 */
function complet(state: Partial<PlayerPayload>): PlayerPayload {
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
    year: null,
    duration_s: null,
    origin: null,
    cover_href: null,
    cover_origin: null,
    position_s: null,
    seekable: false,
    can_eject: false,
    ...state,
  }
}

function monteAvec(state: Partial<PlayerPayload> | null) {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(PlayerCard, { props: { state: state ? complet(state) : null, seekStep: 10 } })
}

describe('PlayerCard', () => {
  it('displayed la source dès la première trame', () => {
    const w = monteAvec({ source: 'cd' })
    expect(w.get('[data-source]').text()).toBe('cd')
  })

  it('nomme l absence de source au lieu d afficher un vide', () => {
    // Le coeur demarre desormais sans aucune source (un greffon lent peut
    // s'annoncer bien apres), et le protocole dit cette absence par la chaine
    // vide. Sans label, on lisait « Source active : » suivi de rien — un
    // affichage qu'on prend pour une panne d'IHM. La cle brute suffit ici : le
    // catalogue n'est step load sous test, `createT` rend la cle.
    const w = monteAvec({ source: '' })
    expect(w.find('[data-source]').text()).toBe('no_source')
  })

  it('ne dit step « aucune source » avant la premiere trame', () => {
    // `state` a `null`, c'est « l'state n'est step encore arrive » et non « il n'y
    // a step de source » : annoncer l'absence a cet instant serait faux, et
    // c'est le piege d'un `||` pose sur `state?.source`.
    const w = monteAvec(null)
    expect(w.find('[data-source]').text()).toBe('')
  })

  it('displayed la présélection en cours quand la Source en déclare une', () => {
    const w = monteAvec({ preset: 4 })
    expect(w.get('[data-player-preset]').text()).toBe('4')
  })

  it('n displayed step de ligne de présélection quand la Source n en déclare aucune', () => {
    // `null` couvre deux situations où il n'y a rien à numéroter — rien ne joue,
    // ou la Source ne numérote step (cd sans disque, entrée auxiliaire) — et une
    // ligne vide y laisserait croire à une panne.
    const w = monteAvec({ preset: null })
    expect(w.find('[data-player-preset]').exists()).toBe(false)
  })

  it('displayed la présélection 0 plutôt que de la confondre avec une absence', () => {
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

  it('n displayed que le numéro quand la Source ne nomme rien', () => {
    // Cas du cd : une présélection déclarée (la piste), mais aucun nom — step
    // de clé i18n générique du type « station » qui serait fausse ici.
    const w = monteAvec({ preset: 3, preset_name: null })
    expect(w.find('[data-player-preset]').text()).toBe('3')
    expect(w.find('[data-player-preset-name]').exists()).toBe(false)
  })

  it('displayed le statut déclaré par la source', () => {
    const w = monteAvec({ status: 'PAS DE DISQUE' })
    expect(w.find('[data-player-status]').text()).toBe('PAS DE DISQUE')
  })

  it('n displayed aucune ligne de statut quand il n y en a step', () => {
    const w = monteAvec({ status: null })
    expect(w.find('[data-player-status]').exists()).toBe(false)
  })

  it('masque la ligne de statut en veille pour ne step doubler le badge VEILLE', () => {
    // Le statut publié en veille est le même mot du même catalogue que le
    // badge "VEILLE" affiché juste au-dessus (voir M2, revue de branche) :
    // sans ce masquage, la carte montrerait "VEILLE" deux fois, la seconde
    // sans libellé contrairement à ses voisines ("Présélection :", "Volume :").
    const w = monteAvec({ status: 'VEILLE', standby: true })
    expect(w.find('[data-player-status]').exists()).toBe(false)
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('signale la veille', () => {
    const w = monteAvec({ standby: true })
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('n displayed step de veille quand elle est inactive', () => {
    const w = monteAvec({ standby: false })
    expect(w.find('[data-standby]').exists()).toBe(false)
  })

  it('n displayed step de bloc morceau tant que rien n est connu', () => {
    // La plupart des stations francaises n'annoncent rien : un bloc « En
    // ecoute » vide ferait croire a une panne. L'encart du player, lui, reste.
    const w = monteAvec(null)
    expect(w.find('[data-player]').exists()).toBe(true)
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('n displayed step de bloc morceau pour une duration seule', () => {
    const w = monteAvec({ duration_s: 214 })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('ajoute artiste, titre, album, duration et origine quand ils arrivent', () => {
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
    expect(w.find('[data-duration]').text()).toBe('9:05')
  })

  it('displayed un titre seul, tel que le donne l en-tete ICY', () => {
    // L'ICY livre une chaine unique, non decoupee : elle arrive dans `title`.
    // Les webradios OUI FM l'emettent meme dans l'order « Titre - ARTISTE ».
    const w = monteAvec({ title: 'Made Up - TAHITI 80', origin: 'icy' })
    expect(w.find('[data-titre]').text()).toBe('Made Up - TAHITI 80')
    expect(w.find('[data-artiste]').exists()).toBe(false)
  })

  it('displayed l artiste seul quand le titre manque', () => {
    // Decision du proprietaire : toute information disponible est displayed.
    const w = monteAvec({ artist: 'Téléphone', origin: 'ouifm-metas' })
    expect(w.find('[data-artiste]').text()).toBe('Téléphone')
    expect(w.find('[data-titre]').exists()).toBe(false)
  })

  it('retire le bloc morceau quand la lecture s stopped', async () => {
    // Changement d'identite ou arret : le coeur diffuse un state sans morceau,
    // et l'ancien titre ne doit step rester a l'ecran — mais l'encart du player
    // reste, lui, avec la source (le volume vit desormais dans le slot
    // `commandes`, hors de cette carte).
    const w = monteAvec({ title: 'premier' })
    await w.setProps({ state: complet({}) })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.find('[data-player]').exists()).toBe(true)
  })

  it('montre la barre quand une position est connue', () => {
    const w = mount(PlayerCard, {
      props: {
        state: complet({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-barre]').exists()).toBe(true)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('n displayed step la duration en en-tete quand la barre la porte deja', () => {
    // Defaut corrige : titre + position connus (le cas nominal d'un CD
    // reconnu) affichait "4:14" en en-tete ET "1:27 ... 4:14" dans la barre,
    // deux fois la meme information.
    const w = mount(PlayerCard, {
      props: {
        state: complet({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-duration]').exists()).toBe(false)
    expect(w.get('[data-duration-totale]').text()).toBe('4:14')
  })

  it("displayed la pochette quand l'appareil en sert une", () => {
    const w = monteAvec({ title: 'So What', cover_href: '/api/cover/1a2b', cover_origin: 'files' })
    const img = w.find('[data-pochette] img')
    expect(img.exists()).toBe(true)
    // L'IHM ne doit jamais pointer vers l'exterieur : le coeur sert l'image.
    // Et elle demande la **vignette** : le carre fait 224 px, le `folder.jpg`
    // d'un NAS en fait couramment trois mebioctets.
    expect(img.attributes('src')).toBe('/api/cover/1a2b?taille=vignette')
    // L'origine de la pochette n'est plus un badge : elle est dans la popin
    // de provenance, avec les autres fields (voir plus bas).
    expect(w.find('[data-cover-origin]').exists()).toBe(false)
  })

  it('detaille la provenance champ par champ dans une popin', async () => {
    // **Ce que les deux badges d'origine ne disaient step.** Ils nommaient le
    // contributeur du text et celui de l'image ; l'ecran est compose de plus
    // de mains que ca — le titre d'ici, l'annee de la, la pochette d'ailleurs
    // — et c'est cette question-la qu'on se pose devant un titre faux.
    const w = monteAvec({
      title: 'So What',
      origin: 'icy',
      cover_href: '/api/cover/1a2b',
      cover_origin: 'musicbrainz',
      provenance: {
        fields: { title: 'icy', year: 'musicbrainz', cover: 'musicbrainz' },
        misses: ['ouifm-metas'],
      },
    })
    // Les badges ont cede la place au bouton.
    expect(w.find('[data-origin]').exists()).toBe(false)
    expect(w.find('[data-cover-origin]').exists()).toBe(false)

    await w.get('[data-provenance-ouvrir]').trigger('click')
    const popin = document.body.querySelector('[data-provenance-popin]')
    expect(popin).not.toBeNull()
    const par = (champ: string) =>
      popin?.querySelector(`[data-provenance-champ="${champ}"]`)?.textContent?.trim()
    expect(par('title')).toBe('icy')
    expect(par('year')).toBe('musicbrainz')
    expect(par('cover')).toBe('musicbrainz')
    // « A cherche sans rien trouver » est une section a part : ce n'est step la
    // meme information qu'une absence de la list ci-dessus, qui vaut aussi
    // quand le greffon n'a jamais ete interroge.
    expect(popin?.querySelector('[data-provenance-misses]')?.textContent).toContain('ouifm-metas')
    w.unmount()
  })

  it('nomme le retravail a cote de la source, jamais a sa place', async () => {
    // **Le defaut signale par le proprietaire** : sur une radio sans greffon de
    // metadonnees, l'ICY donnait l'information, `musicbrainz` la decoupait, et
    // l'ecran affichait « Titre : musicbrainz ». La station est la source ; le
    // decoupage se dit a cote.
    const w = monteAvec({
      title: 'Miles Davis - So What',
      provenance: {
        fields: { title: 'icy', artist: 'icy' },
        derived: { title: 'musicbrainz', artist: 'musicbrainz' },
      },
    })
    await w.get('[data-provenance-ouvrir]').trigger('click')
    const popin = document.body.querySelector('[data-provenance-popin]')
    const ligne = popin?.querySelector('[data-provenance-champ="title"]')
    // La source, mot pour mot : c'est elle qui etait effacee.
    expect(ligne?.textContent).toContain('icy')
    // Et le retravail existe, **dans la meme ligne**. Le label lui-meme vient
    // du catalogue, que ce montage ne load step (`t()` retombe sur la cle) :
    // sa redaction et la parite fr/en sont couvertes par `i18nKeysUsed`, ce
    // test-ci ne prouve que l'agencement.
    expect(ligne?.querySelector('[data-provenance-derive="title"]')).not.toBeNull()
    w.unmount()
  })

  it("n'offre step le bouton quand il n'y a rien a expliquer", () => {
    // Un `(?)` qui ouvre une popin vide promet une explication et n'en donne
    // aucune : c'est le cas ordinaire avant qu'un morceau ne soit identifie.
    const w = monteAvec({ title: 'Made Up - TAHITI 80' })
    expect(w.find('[data-provenance-ouvrir]').exists()).toBe(false)
  })

  it('garde le carre en place quand il n y a step de pochette', () => {
    const w = monteAvec({ title: 'So What' })
    // Le carre existe toujours : la pochette arrive apres le text, parfois
    // plusieurs secondes apres, et un carre qui apparait decalerait tout.
    expect(w.find('[data-pochette]').exists()).toBe(true)
    expect(w.find('[data-pochette] img').exists()).toBe(false)
    expect(w.find('[data-pochette-repli]').exists()).toBe(true)
  })

  it('retombe sur le repli quand le browser ne peut step charger la pochette', async () => {
    // Le cas reel : la cle du cache du coeur est bornee a quelques entries, et
    // le fichier lui-meme vit sur un partage qui peut disparaitre — les deux
    // rendent un 404 sous une URL deja publiee. Sans `@error`, le carre
    // reserve montrait le glyphe d'image cassee du browser au lieu du repli
    // ♫ prevu pour exactement cette situation.
    const w = monteAvec({ title: 'So What', cover_href: '/api/cover/1a2b' })
    await w.get('[data-pochette] img').trigger('error')
    expect(w.find('[data-pochette] img').exists()).toBe(false)
    expect(w.find('[data-pochette-repli]').exists()).toBe(true)
    // Le carre lui-meme ne bouge step : rien ne doit se decaler.
    expect(w.find('[data-pochette]').exists()).toBe(true)

    // Et une **autre** image redonne sa chance a l'element : sans cela, un
    // seul echec condamnerait le carre pour le reste de la session.
    await w.setProps({ state: complet({ title: 'So What', cover_href: '/api/cover/3c4d' }) })
    expect(w.get('[data-pochette] img').attributes('src')).toBe(
      '/api/cover/3c4d?taille=vignette',
    )
  })

  it('agrandit la pochette au clic, et la referme au clic suivant', async () => {
    const w = monteAvec({ title: 'So What', cover_href: '/api/cover/1a2b' })
    // Rien d'open au depart.
    expect(document.body.querySelector('[data-pochette-enlarged]')).toBeNull()

    await w.get('[data-pochette-agrandir]').trigger('click')
    const surcouche = document.body.querySelector('[data-pochette-enlarged]')
    expect(surcouche).not.toBeNull()
    // La vue enlarged load l'image **pleine**, step la vignette : c'est tout
    // l'interet d'agrandir.
    expect(surcouche?.querySelector('img')?.getAttribute('src')).toBe('/api/cover/1a2b')

    // La surcouche est **teleportee vers le body** : elle n'appartient step au
    // sous-arbre du wrapper, donc `w.get` ne la voit step. On la pilote par le
    // DOM, comme le ferait un vrai clic.
    const fermer = document.body.querySelector<HTMLElement>('[data-pochette-fermer]')
    expect(fermer).not.toBeNull()
    fermer!.click()
    await nextTick()
    expect(document.body.querySelector('[data-pochette-enlarged]')).toBeNull()
    w.unmount()
  })

  it('referme la pochette enlarged quand la piste change', async () => {
    // Sinon l'image de la piste suivante s'displayed en plein ecran sans que
    // personne l'ait demande.
    const w = monteAvec({ title: 'So What', cover_href: '/api/cover/1a2b' })
    await w.get('[data-pochette-agrandir]').trigger('click')
    expect(document.body.querySelector('[data-pochette-enlarged]')).not.toBeNull()

    await w.setProps({ state: complet({ title: 'Blue in Green', cover_href: '/api/cover/9f9f' }) })
    expect(document.body.querySelector('[data-pochette-enlarged]')).toBeNull()
    w.unmount()
  })

  it("n'offre step d'agrandissement quand il n'y a step de pochette", () => {
    // Un bouton qui n'ouvre rien est pire qu'aucun bouton : le repli ♫ n'est
    // step une image.
    const w = monteAvec({ title: 'So What' })
    expect(w.find('[data-pochette-agrandir]').exists()).toBe(false)
  })

  it('ne montre rien de la progression quand aucune position n est connue', () => {
    const w = mount(PlayerCard, {
      props: {
        state: complet({ title: 'Bikwix', position_s: null, duration_s: 254 }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-position]').exists()).toBe(false)
  })

  // Sans titre ni artiste ni album, le bloc « en ecoute » est masque : la
  // progression, elle, doit rester visible. C'est le cas d'un fichier sans
  // etiquettes ou d'un disque que MusicBrainz ne reconnait step, ou mpv
  // connait pourtant parfaitement la position.
  it('montre la progression meme sans aucune metadonnee', () => {
    const w = mount(PlayerCard, {
      props: {
        state: complet({ position_s: 87, duration_s: 254, seekable: true }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.get('[data-position]').text()).toBe('1:27')
    expect(w.find('[data-barre]').exists()).toBe(true)
  })

  it('la pochette et le morceau sont au centre, la source en pastille', () => {
    const w = monteAvec({ title: 'Blue in Green', artist: 'Miles Davis', album: 'Kind of Blue', preset: 1, preset_name: 'FIP' })
    expect(w.get('[data-source]').text()).toBe('radio')
    expect(w.get('[data-player-preset]').text()).toBe('1')
    expect(w.get('[data-player-preset-name]').text()).toBe('FIP')
    expect(w.get('[data-titre]').classes()).toContain('text-xl')
    expect(w.find('[data-pochette]').exists()).toBe(true)
  })

  it('le carre de pochette reste la meme sans morceau : c est lui qui tient la mise en page', () => {
    const w = monteAvec({ status: 'NO DISC', preset_count: 0 })
    expect(w.find('[data-pochette]').exists()).toBe(true)
    expect(w.find('[data-pochette-repli]').exists()).toBe(true)
    expect(w.get('[data-player-status]').text()).toBe('NO DISC')
  })

  it('en veille la pochette s eteint', () => {
    const w = monteAvec({ standby: true })
    expect(w.get('[data-pochette]').classes()).toContain('opacity-50')
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('rend les slots actions et commandes', () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const w = mount(PlayerCard, {
      props: { state: complet({}), seekStep: 10 },
      slots: { actions: '<button data-test-action>a</button>', commandes: '<div data-test-commandes>c</div>' },
    })
    expect(w.find('[data-slot="card-action"] [data-test-action]').exists()).toBe(true)
    expect(w.find('[data-test-commandes]').exists()).toBe(true)
  })

  describe('année', () => {
    it("s'accole à l'album, séparée par un point médian", () => {
      const w = monteAvec({ title: 'So What', album: 'Kind of Blue', year: 1959 })
      expect(w.find('[data-album]').text()).toBe('Kind of Blue')
      expect(w.find('[data-annee]').text()).toBe('1959')
      // Les deux dans la meme ligne, avec le separateur entre eux.
      expect(w.find('[data-album]').element.parentElement?.textContent).toContain('Kind of Blue · 1959')
    })

    it('sort seule quand aucun album n’est connu', () => {
      // Reel : un flux peut donner l'annee sans l'album, la grille Radio France
      // rend l'une bien plus souvent que l'autre.
      const w = monteAvec({ title: 'Fire', album: null, year: 1960 })
      expect(w.find('[data-annee]').text()).toBe('1960')
      expect(w.find('[data-album]').exists()).toBe(false)
    })

    it('ne laisse aucune trace quand elle est inconnue', () => {
      const w = monteAvec({ title: 'So What', album: 'Kind of Blue' })
      expect(w.find('[data-annee]').exists()).toBe(false)
    })
  })

  describe('links de plateformes', () => {
    it('rend une icône par plateforme, en lien externe sûr', () => {
      const w = monteAvec({
        title: 'Get Lucky',
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=5NV6Rdv1a3I' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/9956167' },
        ],
      })
      expect(w.findAll('[data-lien]')).toHaveLength(2)
      const yt = w.get('[data-lien="youtube"]')
      expect(yt.attributes('href')).toBe('https://www.youtube.com/watch?v=5NV6Rdv1a3I')
      expect(yt.attributes('target')).toBe('_blank')
      // `noopener` : la cible est un tiers. `noreferrer` : il n'a step a savoir
      // d'ou on vient.
      expect(yt.attributes('rel')).toBe('noopener noreferrer')
      // Un nom accessible traduit, step une icon muette.
      expect(yt.attributes('aria-label')).toBeTruthy()
      expect(yt.find('svg').exists()).toBe(true)
      expect(w.find('[data-lien="deezer"]').exists()).toBe(true)
    })

    it('distingue les trois plateformes par leur icône', () => {
      const w = monteAvec({
        title: 'Get Lucky',
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/1' },
          { platform: 'apple_music', url: 'https://music.apple.com/us/song/1' },
        ],
      })
      // Assertion **par plateforme** et non « trois icons distinctes » : trois
      // icons differentes peuvent tres bien etre les trois mauvaises (deux
      // branches d'un `v-if` inversees passent l'ancienne version du test).
      // La color de marque n'appartient qu'a une des trois icons.
      for (const [plateforme, color] of Object.entries(COULEUR_ICONE)) {
        expect(w.get(`[data-lien="${plateforme}"] svg`).html()).toContain(`fill="${color}"`)
      }
      const svg = w.findAll('[data-lien] svg').map((s) => s.html())
      expect(new Set(svg).size).toBe(3)
    })

    it('rend les icônes sur la même ligne que les badges d’origine', () => {
      // Decision du proprietaire : une ligne a elles seules decalait trop le
      // curseur de volume sur phone. La ligne des badges les accueille.
      const w = monteAvec({
        title: 'Get Lucky',
        duration_s: 248,
        provenance: { fields: { title: 'musicbrainz' } },
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/1' },
          { platform: 'apple_music', url: 'https://music.apple.com/us/song/1' },
        ],
      })
      const ligne = w.get('[data-badges]').element
      expect(w.get('[data-links]').element.parentElement).toBe(ligne)
      // Le bouton de provenance a pris la place des deux badges d'origine.
      expect(w.get('[data-provenance-ouvrir]').element.parentElement).toBe(ligne)
      expect(w.get('[data-duration]').element.parentElement).toBe(ligne)
    })

    it('donne aux ancres une cible tactile de 44 px', () => {
      // 44 px, la cible minimale recommandee au doigt : l'icon seule (20 px)
      // se rate une fois sur trois depuis le canape.
      const w = monteAvec({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      expect(w.get('[data-lien="youtube"]').classes()).toContain('size-11')
      expect(w.get('[data-lien="youtube"] svg').classes()).toContain('size-5')
    })

    it('passe devant le debordement du curseur de la barre de progression', () => {
      // La zone de contact de 44 px du curseur deborde de 19 px au-dessus de
      // sa piste (voir ProgressBar.vue), alors que cette ligne n'est
      // qu'a 8 px plus haut : sans `relative z-10`, un tap au bas d'une
      // ancre de lien tomberait sur le curseur (un SeekTo) plutot que sur le
      // lien. jsdom ne peint rien : ce test documente l'agencement, il ne le
      // prouve step a l'ecran (mesure par le controleur via Playwright).
      const w = monteAvec({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      const classes = w.get('[data-links]').classes()
      expect(classes).toContain('relative')
      expect(classes).toContain('z-10')
    })

    it('reserve la hauteur de la ligne meme sans lien', () => {
      // Sans hauteur minimale, l'arrivee tardive d'un lien (MusicBrainz repond
      // apres le titre) faisait grandir la carte et descendre le volume sous
      // le doigt deja pose.
      const w = monteAvec({
        title: 'Get Lucky',
        provenance: { fields: { title: 'icy' } },
      })
      expect(w.get('[data-badges]').classes()).toContain('min-h-11')
    })

    it('n’ouvre step la ligne des badges quand il n’y a rien a y mettre', () => {
      // Un titre nu (le cas ICY le plus courant) ne doit step reserver 44 px
      // vides sous l'album.
      const w = monteAvec({ title: 'Made Up - TAHITI 80' })
      expect(w.find('[data-badges]').exists()).toBe(false)
    })

    it('ouvre la ligne des badges pour un lien seul', () => {
      const w = monteAvec({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      expect(w.find('[data-badges]').exists()).toBe(true)
    })

    it('ne rend rien pour une plateforme inconnue', () => {
      // Le protocole ferme l'ensemble, mais un `v-else` rendait l'icon Apple
      // pour tout ce qui n'etait ni YouTube ni Deezer : un greffon en avance
      // sur le coeur aurait displayed « Ecouter sur Apple Music » vers Spotify.
      const w = monteAvec({
        title: 'Get Lucky',
        links: [{ platform: 'inconnue' as 'youtube', url: 'https://exemple.test/x' }],
      })
      expect(w.findAll('[data-lien]')).toHaveLength(0)
    })

    it('rend deux ancres pour deux links d’une même plateforme', () => {
      // Rien dans le protocole n'interdit deux links de la meme plateforme :
      // une cle de rendu posee sur `platform` en aurait perdu un.
      const w = monteAvec({
        title: 'Get Lucky',
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=b' },
        ],
      })
      expect(w.findAll('[data-lien="youtube"]')).toHaveLength(2)
    })

    it("n'displayed step la rangée quand il n'y a aucun lien", () => {
      expect(monteAvec({ title: 'So What' }).find('[data-links]').exists()).toBe(false)
      expect(monteAvec({ title: 'So What', links: [] }).find('[data-links]').exists()).toBe(false)
    })

    it('reste mute quand on ne sait rien du morceau par ailleurs', () => {
      // Toute la zone est derriere `nothingToShow` : des icons de plateformes
      // seules, sans titre ni artiste, seraient des boutons sans sujet. Regle
      // heritee du composant, verifiee ici parce que les links sont la
      // premiere donnee qui pourrait arriver seule.
      const w = monteAvec({ links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }] })
      expect(w.find('[data-links]').exists()).toBe(false)
    })
  })
})
