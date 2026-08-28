import { mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import PlayerCard from './PlayerCard.vue'
import type { PlayerPayload } from '../types'

// La couleur de marque : l'exception assumee a la regle « aucune couleur en
// dur » (decision du proprietaire, voir docs/interface.md § Player card).
// Verifier qu'elle est bien la, plateforme par plateforme, documente
// l'exception autant que ca ne la prouve.
const COULEUR_ICONE = {
  youtube: '#FF0000',
  deezer: '#A238FF',
  apple_music: '#FA243C',
} as const

// jsdom ne fournit pas ResizeObserver ; reka-ui l'utilise pour mesurer la
// piste du curseur de BarreProgression, monte ici des que `seekable` est vrai
// (voir web/kit/src/index.test.ts et BarreProgression.test.ts).
beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
})

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
    year: null,
    duration_s: null,
    origin: null,
    cover_href: null,
    cover_origin: null,
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
  it('affiche la source dès la première trame', () => {
    const w = monteAvec({ source: 'cd' })
    expect(w.get('[data-source]').text()).toBe('cd')
  })

  it('nomme l absence de source au lieu d afficher un vide', () => {
    // Le coeur demarre desormais sans aucune source (un greffon lent peut
    // s'annoncer bien apres), et le protocole dit cette absence par la chaine
    // vide. Sans libelle, on lisait « Source active : » suivi de rien — un
    // affichage qu'on prend pour une panne d'IHM. La cle brute suffit ici : le
    // catalogue n'est pas charge sous test, `createT` rend la cle.
    const w = monteAvec({ source: '' })
    expect(w.find('[data-source]').text()).toBe('no_source')
  })

  it('ne dit pas « aucune source » avant la premiere trame', () => {
    // `etat` a `null`, c'est « l'etat n'est pas encore arrive » et non « il n'y
    // a pas de source » : annoncer l'absence a cet instant serait faux, et
    // c'est le piege d'un `||` pose sur `etat?.source`.
    const w = monteAvec(null)
    expect(w.find('[data-source]').text()).toBe('')
  })

  it('affiche la présélection en cours quand la Source en déclare une', () => {
    const w = monteAvec({ preset: 4 })
    expect(w.get('[data-player-preset]').text()).toBe('4')
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

  it('signale la veille', () => {
    const w = monteAvec({ standby: true })
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('n affiche pas de veille quand elle est inactive', () => {
    const w = monteAvec({ standby: false })
    expect(w.find('[data-standby]').exists()).toBe(false)
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
    // reste, lui, avec la source (le volume vit desormais dans le slot
    // `commandes`, hors de cette carte).
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

  it("affiche la pochette quand l'appareil en sert une", () => {
    const w = monteAvec({ title: 'So What', cover_href: '/api/cover/1a2b', cover_origin: 'files' })
    const img = w.find('[data-pochette] img')
    expect(img.exists()).toBe(true)
    // L'IHM ne doit jamais pointer vers l'exterieur : le coeur sert l'image.
    expect(img.attributes('src')).toBe('/api/cover/1a2b')
    expect(w.find('[data-cover-origin]').text()).toContain('files')
  })

  it('ne repete pas le meme contributeur sur la pochette', () => {
    // Le cas courant sur une radio : un seul greffon fournit le texte et
    // l'image, et les deux badges affichaient alors le meme mot cote a cote.
    // Le badge de gauche dit deja qui c'est.
    const w = monteAvec({
      title: 'So What',
      origin: 'radiofrance-metas',
      cover_href: '/api/cover/1a2b',
      cover_origin: 'radiofrance-metas',
    })
    expect(w.get('[data-origin]').text()).toBe('radiofrance-metas')
    expect(w.find('[data-cover-origin]').exists()).toBe(false)
  })

  it('montre l origine de la pochette quand elle differe de celle du texte', () => {
    // Le controle de la regle ci-dessus, et ce pour quoi le badge existe :
    // `icy` donne le titre du flux, `musicbrainz` la pochette. Sans ce test,
    // masquer le doublon pourrait degenerer en ne jamais rien montrer, et la
    // suite resterait verte.
    const w = monteAvec({
      title: 'So What',
      origin: 'icy',
      cover_href: '/api/cover/1a2b',
      cover_origin: 'musicbrainz',
    })
    expect(w.get('[data-origin]').text()).toBe('icy')
    expect(w.get('[data-cover-origin]').text()).toBe('musicbrainz')
  })

  it('garde le carre en place quand il n y a pas de pochette', () => {
    const w = monteAvec({ title: 'So What' })
    // Le carre existe toujours : la pochette arrive apres le texte, parfois
    // plusieurs secondes apres, et un carre qui apparait decalerait tout.
    expect(w.find('[data-pochette]').exists()).toBe(true)
    expect(w.find('[data-pochette] img').exists()).toBe(false)
    expect(w.find('[data-pochette-repli]').exists()).toBe(true)
  })

  it('retombe sur le repli quand le navigateur ne peut pas charger la pochette', async () => {
    // Le cas reel : la cle du cache du coeur est bornee a quelques entrees, et
    // le fichier lui-meme vit sur un partage qui peut disparaitre — les deux
    // rendent un 404 sous une URL deja publiee. Sans `@error`, le carre
    // reserve montrait le glyphe d'image cassee du navigateur au lieu du repli
    // ♫ prevu pour exactement cette situation.
    const w = monteAvec({ title: 'So What', cover_href: '/api/cover/1a2b' })
    await w.get('[data-pochette] img').trigger('error')
    expect(w.find('[data-pochette] img').exists()).toBe(false)
    expect(w.find('[data-pochette-repli]').exists()).toBe(true)
    // Le carre lui-meme ne bouge pas : rien ne doit se decaler.
    expect(w.find('[data-pochette]').exists()).toBe(true)

    // Et une **autre** image redonne sa chance a l'element : sans cela, un
    // seul echec condamnerait le carre pour le reste de la session.
    await w.setProps({ etat: complet({ title: 'So What', cover_href: '/api/cover/3c4d' }) })
    expect(w.get('[data-pochette] img').attributes('src')).toBe('/api/cover/3c4d')
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
      props: { etat: complet({}), pasDeplacement: 10 },
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

  describe('liens de plateformes', () => {
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
      // `noopener` : la cible est un tiers. `noreferrer` : il n'a pas a savoir
      // d'ou on vient.
      expect(yt.attributes('rel')).toBe('noopener noreferrer')
      // Un nom accessible traduit, pas une icone muette.
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
      // Assertion **par plateforme** et non « trois icones distinctes » : trois
      // icones differentes peuvent tres bien etre les trois mauvaises (deux
      // branches d'un `v-if` inversees passent l'ancienne version du test).
      // La couleur de marque n'appartient qu'a une des trois icones.
      for (const [plateforme, couleur] of Object.entries(COULEUR_ICONE)) {
        expect(w.get(`[data-lien="${plateforme}"] svg`).html()).toContain(`fill="${couleur}"`)
      }
      const svg = w.findAll('[data-lien] svg').map((s) => s.html())
      expect(new Set(svg).size).toBe(3)
    })

    it('rend les icônes sur la même ligne que les badges d’origine', () => {
      // Decision du proprietaire : une ligne a elles seules decalait trop le
      // curseur de volume sur telephone. La ligne des badges les accueille.
      const w = monteAvec({
        title: 'Get Lucky',
        origin: 'musicbrainz',
        duration_s: 248,
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/1' },
          { platform: 'apple_music', url: 'https://music.apple.com/us/song/1' },
        ],
      })
      const ligne = w.get('[data-badges]').element
      expect(w.get('[data-liens]').element.parentElement).toBe(ligne)
      expect(w.get('[data-origin]').element.parentElement).toBe(ligne)
      expect(w.get('[data-duree]').element.parentElement).toBe(ligne)
    })

    it('donne aux ancres une cible tactile de 44 px', () => {
      // 44 px, la cible minimale recommandee au doigt : l'icone seule (20 px)
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
      // sa piste (voir BarreProgression.vue), alors que cette ligne n'est
      // qu'a 8 px plus haut : sans `relative z-10`, un tap au bas d'une
      // ancre de lien tomberait sur le curseur (un SeekTo) plutot que sur le
      // lien. jsdom ne peint rien : ce test documente l'agencement, il ne le
      // prouve pas a l'ecran (mesure par le controleur via Playwright).
      const w = monteAvec({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      const classes = w.get('[data-liens]').classes()
      expect(classes).toContain('relative')
      expect(classes).toContain('z-10')
    })

    it('reserve la hauteur de la ligne meme sans lien', () => {
      // Sans hauteur minimale, l'arrivee tardive d'un lien (MusicBrainz repond
      // apres le titre) faisait grandir la carte et descendre le volume sous
      // le doigt deja pose.
      const w = monteAvec({ title: 'Get Lucky', origin: 'icy' })
      expect(w.get('[data-badges]').classes()).toContain('min-h-11')
    })

    it('n’ouvre pas la ligne des badges quand il n’y a rien a y mettre', () => {
      // Un titre nu (le cas ICY le plus courant) ne doit pas reserver 44 px
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
      // Le protocole ferme l'ensemble, mais un `v-else` rendait l'icone Apple
      // pour tout ce qui n'etait ni YouTube ni Deezer : un greffon en avance
      // sur le coeur aurait affiche « Ecouter sur Apple Music » vers Spotify.
      const w = monteAvec({
        title: 'Get Lucky',
        links: [{ platform: 'inconnue' as 'youtube', url: 'https://exemple.test/x' }],
      })
      expect(w.findAll('[data-lien]')).toHaveLength(0)
    })

    it('rend deux ancres pour deux liens d’une même plateforme', () => {
      // Rien dans le protocole n'interdit deux liens de la meme plateforme :
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

    it("n'affiche pas la rangée quand il n'y a aucun lien", () => {
      expect(monteAvec({ title: 'So What' }).find('[data-liens]').exists()).toBe(false)
      expect(monteAvec({ title: 'So What', links: [] }).find('[data-liens]').exists()).toBe(false)
    })

    it('reste muet quand on ne sait rien du morceau par ailleurs', () => {
      // Toute la zone est derriere `riendAfficher` : des icones de plateformes
      // seules, sans titre ni artiste, seraient des boutons sans sujet. Regle
      // heritee du composant, verifiee ici parce que les liens sont la
      // premiere donnee qui pourrait arriver seule.
      const w = monteAvec({ links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }] })
      expect(w.find('[data-liens]').exists()).toBe(false)
    })
  })
})
