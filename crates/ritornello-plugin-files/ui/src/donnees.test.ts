import { describe, expect, it } from 'vitest'
import {
  cibleRacine,
  feuille,
  formaterDuree,
  normaliserBrowse,
  normaliserDonnees,
  normaliserRacine,
  tronquerDebut,
} from './donnees'

describe('normalisation des racines', () => {
  it('comble les champs que le plugin omet quand ils sont vides', () => {
    // Régression encodée : `Root` est sérialisé avec
    // `skip_serializing_if = "Option::is_none"`, donc `subpath` **disparaît**
    // du corps au lieu d'y figurer à vide. Une vue qui appellerait
    // `r.subpath.trim()` planterait dans un `computed`, et un `computed` qui
    // lève laisse la page à moitié rendue, sans message.
    const r = normaliserRacine({ name: 'nas', kind: 'smb', host: 'h', share: 's' })
    expect(r.subpath).toBe('')
    expect(r.user).toBe('')
    expect(r.writable).toBe(false)
    expect(r.mounted).toBe(false)
  })

  it('traite tout genre inconnu comme un partage', () => {
    expect(normaliserRacine({ kind: 'local' }).kind).toBe('local')
    expect(normaliserRacine({}).kind).toBe('smb')
  })
})

describe('normalisation d’un parcours', () => {
  it('recolle le chemin de chaque entrée, le plugin ne rendant que des noms', () => {
    // Régression encodée : `scan::list_dir` rend `dirs` et `files` comme de
    // **simples noms**, relatifs au répertoire lu. Une page qui les prendrait
    // pour des chemins renverrait `Jazz` au lieu d'`Albums/Jazz` dans le
    // `browse` suivant — donc un autre dossier, ou un refus.
    const nav = normaliserBrowse({
      root: 'nas',
      path: 'Albums',
      dirs: ['Jazz'],
      files: ['01.mp3'],
      results: [],
    })
    expect(nav.entrees).toEqual([
      { name: 'Jazz', path: 'Albums/Jazz', dir: true, playlist: false },
      { name: '01.mp3', path: 'Albums/01.mp3', dir: false, playlist: false },
    ])
  })

  it('ne préfixe rien au niveau supérieur, dont le chemin est vide', () => {
    const nav = normaliserBrowse({ root: 'nas', path: '', dirs: ['Albums'], files: [] })
    expect(nav.entrees).toEqual([{ name: 'Albums', path: 'Albums', dir: true, playlist: false }])
  })

  it('prend les résultats de recherche pour des chemins complets', () => {
    // À l'inverse d'un niveau : une recherche traverse l'arborescence, ses
    // trouvailles ne sont pas dans le répertoire courant.
    const nav = normaliserBrowse({
      root: 'nas',
      path: '',
      dirs: [],
      files: [],
      results: ['Albums/Jazz/miles.flac'],
      truncated: true,
    })
    expect(nav.resultats).toEqual([
      { name: 'miles.flac', path: 'Albums/Jazz/miles.flac', dir: false, playlist: false },
    ])
    expect(nav.tronque).toBe(true)
  })

  it('rend un parcours vide plutôt que de lever sur un champ absent', () => {
    expect(normaliserBrowse(undefined).entrees).toEqual([])
    expect(normaliserBrowse({}).tronque).toBe(false)
  })

  it('place les listes de lecture avant les pistes, et les marque comme telles', () => {
    // Elles portent une action différente — charger, non ajouter — et une liste
    // noyée sous cent fichiers ne se voit pas, alors que c'est souvent elle
    // qu'on cherche dans un dossier d'album.
    const nav = normaliserBrowse({
      root: 'nas',
      path: 'Albums',
      dirs: ['Jazz'],
      files: ['01.mp3'],
      playlists: ['tout.m3u'],
      results: [],
    })
    expect(nav.entrees.map((e) => e.name)).toEqual(['Jazz', 'tout.m3u', '01.mp3'])
    expect(nav.entrees[1]).toEqual({
      name: 'tout.m3u',
      path: 'Albums/tout.m3u',
      dir: false,
      playlist: true,
    })
  })

  it('un plugin plus ancien, sans le champ des listes, ne casse rien', () => {
    // Pendant un déploiement, le binaire peut précéder la page ou l'inverse.
    const nav = normaliserBrowse({ root: 'n', path: '', dirs: [], files: ['a.mp3'] })
    expect(nav.entrees.map((e) => e.playlist)).toEqual([false])
  })
})

describe('normalisation de la charge complète', () => {
  it('accepte un corps minimal sans lever', () => {
    const d = normaliserDonnees({})
    expect(d.roots).toEqual([])
    expect(d.playlist).toEqual([])
    expect(d.scan).toEqual({ running: false, found: 0, dir: '', error: '' })
    expect(d.unresolved).toEqual([])
  })

  it('reprend l’incident du dernier balayage, qui survit à sa fin', () => {
    // `add_dir` rend la main bien avant la fin de la marche récursive : c'est
    // le seul endroit où la page peut apprendre qu'un ajout a échoué.
    const d = normaliserDonnees({
      scan: { running: false, found: 0, dir: '', error: 'could not read "Albums"' },
    })
    expect(d.scan.error).toBe('could not read "Albums"')
  })

  it('replie une piste sans nom sur le dernier segment de son chemin', () => {
    const d = normaliserDonnees({ playlist: [{ path: 'Albums/Jazz/01.mp3' }] })
    expect(d.playlist[0]).toEqual({
      path: 'Albums/Jazz/01.mp3',
      name: '01.mp3',
      duration_s: 0,
      missing: false,
    })
  })
})

describe('mise en forme', () => {
  it('rend une durée inconnue par un tiret, jamais par « 0:00 »', () => {
    // « 0:00 » affirmerait une piste vide ; le tiret dit qu'on ne sait pas.
    expect(formaterDuree(0)).toBe('—')
    expect(formaterDuree(Number.NaN)).toBe('—')
  })

  it('passe aux heures au-delà de soixante minutes', () => {
    expect(formaterDuree(245)).toBe('4:05')
    expect(formaterDuree(3725)).toBe('1:02:05')
  })

  it('compose la cible d’une racine selon son genre', () => {
    expect(cibleRacine(normaliserRacine({ kind: 'local', path: '/mnt/usb' }))).toBe('/mnt/usb')
    expect(
      cibleRacine(normaliserRacine({ kind: 'smb', host: 'nas', share: 'musique' })),
    ).toBe('//nas/musique')
    expect(
      cibleRacine(
        normaliserRacine({ kind: 'smb', host: 'nas', share: 'musique', subpath: 'Albums' }),
      ),
    ).toBe('//nas/musique/Albums')
  })

  it('extrait le dernier segment d’un chemin', () => {
    expect(feuille('a/b/c.mp3')).toBe('c.mp3')
    expect(feuille('')).toBe('')
  })
})

describe('troncature d’un chemin par le début', () => {
  it('laisse intact un chemin qui tient', () => {
    expect(tronquerDebut('/media/usb/Albums', 52)).toBe('/media/usb/Albums')
    expect(tronquerDebut('', 52)).toBe('')
  })

  it('coupe le début et garde la fin, qui est l’information utile', () => {
    // C'est tout l'objet de cette fonction : sur un chemin, ce qui compte est le
    // dossier où l'on se trouve, pas la racine d'où l'on vient. `text-overflow`
    // ne sait couper qu'à droite, donc il ferait exactement l'inverse.
    const chemin = '/mnt/c/Users/skerdudou/OneDrive - Klee Group/perso/steven prive/mp3'
    const court = tronquerDebut(chemin, 30)
    expect(court.startsWith('…/')).toBe(true)
    expect(court.endsWith('mp3')).toBe(true)
    expect(court.length).toBeLessThanOrEqual(30)
  })

  it('coupe sur des segments entiers, jamais au milieu d’un nom', () => {
    // « …ents/Ma Musique » est illisible là où « …/Ma Musique » garde un sens.
    // Et elle garde autant de queue que le budget le permet, pas seulement le
    // dernier segment : le contexte immédiat aide à se situer.
    expect(tronquerDebut('/a/bbbb/Documents/Ma Musique', 22)).toBe('…/Documents/Ma Musique')
    expect(tronquerDebut('/a/bbbb/Documents/Ma Musique', 16)).toBe('…/Ma Musique')
  })

  it('un seul nom plus long que le budget est coupé dedans, faute de mieux', () => {
    // Le repli : mieux vaut une fin lisible qu'un affichage qui déborde.
    const court = tronquerDebut('/x/' + 'z'.repeat(80), 20)
    expect(court.length).toBeLessThanOrEqual(20)
    expect(court.startsWith('…')).toBe(true)
    expect(court.endsWith('z')).toBe(true)
  })

  it('garde le préfixe d’un partage quand il tient', () => {
    // Forme composée par l'assistant réseau : le partage doit rester visible,
    // c'est le repère qui manquait.
    expect(tronquerDebut('//192.168.1.15/music/Yann Tiersen', 52)).toBe(
      '//192.168.1.15/music/Yann Tiersen',
    )
  })
})

describe('normalisation des volumes et de l’exploration', () => {
  it('une charge utile sans les champs neufs ne casse pas la page', () => {
    // Pendant un déploiement, le plugin peut être plus ancien que la page :
    // absent doit valoir « rien », jamais un `undefined` qui, traversant un
    // `v-for`, casserait le rendu entier au lieu d'une section vide.
    const d = normaliserDonnees({})
    expect(d.volumes).toEqual([])
    expect(d.canBrowseSmb).toBe(false)
    expect(d.mountError).toBeNull()
    expect(d.explore.open).toBe(false)
    expect(d.explore.kind).toBeNull()
    expect(d.explore.dirs).toEqual([])
    expect(d.explore.shares).toEqual([])
    expect(d.explore.audioCount).toBe(0)
  })

  it('les volumes, la capacité et l’exploration se relisent', () => {
    const d = normaliserDonnees({
      volumes: [{ path: '/media/usb', fstype: 'vfat' }],
      can_browse_smb: true,
      mount_error: 'Interactive authentication required.',
      explore: {
        open: true,
        kind: 'smb',
        host: 'nas',
        share: 'musique',
        path: 'Albums',
        shares: ['musique', 'photo'],
        dirs: ['Jazz'],
        audio_count: 12,
        busy: false,
        error: null,
      },
    })
    expect(d.volumes[0]).toEqual({ path: '/media/usb', fstype: 'vfat' })
    expect(d.canBrowseSmb).toBe(true)
    expect(d.mountError).toBe('Interactive authentication required.')
    expect(d.explore.kind).toBe('smb')
    expect(d.explore.audioCount).toBe(12)
    expect(d.explore.dirs).toEqual(['Jazz'])
    expect(d.explore.shares).toEqual(['musique', 'photo'])
  })

  it('un genre d’assistant inconnu retombe sur « aucun »', () => {
    // Le genre pilote l'affichage de la popin. Une valeur inattendue doit la
    // laisser fermée plutôt qu'ouvrir un panneau à moitié composé.
    expect(normaliserDonnees({ explore: { kind: 'ftp' } }).explore.kind).toBeNull()
  })

  it('une erreur vide vaut « rien à signaler », pas une erreur sans texte', () => {
    // Le plugin rend `null` quand tout va bien ; une chaîne vide dit la même
    // chose. Les lire différemment afficherait un bandeau d'erreur muet.
    expect(normaliserDonnees({ explore: { error: '' } }).explore.error).toBeNull()
    expect(normaliserDonnees({ mount_error: '' }).mountError).toBeNull()
  })
})
