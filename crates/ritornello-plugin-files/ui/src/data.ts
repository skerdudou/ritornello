// Formes de données échangées avec le plugin, et les quelques fonctions pures
// qui les mettent en forme.
//
// Pourquoi normaliser au lieu de consommer le JSON tel quel : le plugin
// sérialise ses structures Rust avec `skip_serializing_if = "Option::is_none"`
// (voir `roots.rs`), donc un champ vide **disparaît** du corps au lieu d'y
// figurer à vide. Une vue qui lirait `r.subpath.trim()` planterait sur la
// première root sans sous-path — et un plantage dans un `computed` de Vue
// laisse la page à moitié rendue, sans message. On ramène donc tout à des
// valeurs totales une seule fois, à la frontière.

/** Genre de root. `local` = répertoire de l'appareil, `smb` = partage réseau. */
export type RootKind = 'local' | 'smb'

export interface Root {
  name: string
  kind: RootKind
  /** Genre `local` uniquement : path absolu. */
  path: string
  host: string
  share: string
  subpath: string
  user: string
  domain: string
  writable: boolean
  /** État observé du montage, rendu par le plugin ; jamais saisi par la page. */
  mounted: boolean
}

export interface Track {
  path: string
  name: string
  duration_s: number
  /**
   * Track dont le fichier n'a pas été retrouvé : marquée, jamais masquée.
   *
   * Trois états, pas deux. `null` veut dire **indéterminé** : le point de
   * montage de la piste ne répondait pas quand le plugin a regardé. Afficher
   * « introuvable » dans ce cas accuserait le fichier d'une panne qui est celle
   * du partage, et enverrait search le défaut au mauvais endroit.
   */
  missing: boolean | null
}

export interface Scan {
  running: boolean
  found: number
  dir: string
  /**
   * Refus ou incident du **dernier** balayage, déjà traduit par le plugin.
   *
   * Il survit à la fin du balayage, et c'est délibéré côté plugin : `add_dir`
   * rend la main bien avant que la marche récursive ne se termine, donc c'est
   * le seul endroit où la page peut apprendre qu'un ajout a échoué. La chaîne
   * vide vaut « rien à signaler ».
   */
  error: string
}

export interface Saved {
  name: string
  /** `internal` ou le nom d'une root. */
  where: string
}

/** Une entrée d'un niveau d'arborescence, path **relatif à la root**. */
export interface Entry {
  name: string
  path: string
  dir: boolean
  /**
   * Fichier de liste de lecture (`.m3u`, `.m3u8`).
   *
   * Exclusif de `dir`. Il porte une action différente des autres : une liste se
   * **charge** — elle remplace la liste en cours — là où un dossier ou une piste
   * s'y ajoutent. Les confondre ferait ajouter un fichier texte que mpv
   * tenterait de jouer.
   */
  playlist: boolean
}

/**
 * Dernier journey ou dernière recherche, tels que le plugin les range.
 *
 * `set_data` ne rend qu'un `Ok`/`Err`, sans charge utile : le contenu voyage
 * par `get_data`, comme la recherche d'annuaire du plugin radio. Les deux
 * opérations écrivent au même endroit — une recherche efface donc le niveau
 * parcouru, et réciproquement.
 */
export interface Navigation {
  root: string
  path: string
  /**
   * Motif de la dernière recherche, vide pour un journey.
   *
   * Ce que la page en fait : distinguer la réponse à SON journey de celle à
   * une recherche portant sur le même dossier — les deux se rangent au même
   * endroit côté plugin.
   */
  query: string
  /** Contenu du niveau `path`, dossiers d'abord puis fichiers. */
  entries: Entry[]
  /** Résultats de la dernière recherche. */
  results: Entry[]
  /** Le plugin a plafonné la recherche : il y en avait davantage. */
  truncated: boolean
  /**
   * Le journey a été interrompu avant d'avoir tout vu, distinct de `truncated`.
   *
   * Deux causes, deux conseils : `truncated` invite à préciser le motif,
   * `abort` invite à descend dans un sous-dossier. Les confondre faisait
   * afficher « Aucun résultat » — donc « ce fichier n'existe pas » — pour une
   * recherche qui avait simplement renoncé avant d'arriver jusqu'à lui.
   */
  abort: boolean
}

/** Un volume monté de l'appareil, tel que le plugin le lit dans `/proc/mounts`. */
export interface Volume {
  path: string
  fstype: string
}

/**
 * L'assistant de déclaration en cours.
 *
 * Emplacement **distinct** de `browse` : la popin et le volet Parcourir sont
 * deux curseurs indépendants, et les faire partager un emplacement ferait
 * qu'open une popin réinitialiserait l'arbre derrière elle.
 *
 * Aucun identifiant n'y figure. Le plugin ne les sérialise jamais : le mot de
 * passe traverse le fil une fois, à la connexion, et vit ensuite dans une
 * session en mémoire du plugin que la page ne relit pas.
 */
export interface Exploration {
  open: boolean
  kind: RootKind | null
  host: string
  share: string
  /** Chemin absolu pour un volume, relatif au partage pour un partage. */
  path: string
  shares: string[]
  dirs: string[]
  /** Fichiers audio du niveau ouvert : c'est ce qui dit qu'on est au bon endroit. */
  audioCount: number
  busy: boolean
  error: string | null
}

const EMPTY_EXPLORATION: Exploration = {
  open: false,
  kind: null,
  host: '',
  share: '',
  path: '',
  shares: [],
  dirs: [],
  audioCount: 0,
  busy: false,
  error: null,
}

export interface Data {
  roots: Root[]
  playlist: Track[]
  index: number
  scan: Scan
  saved: Saved[]
  unresolved: string[]
  browse: Navigation
  volumes: Volume[]
  /** `smbclient` est-il utilisable. Faux grise l'assistant réseau, sans le remove. */
  canBrowseSmb: boolean
  /**
   * Cette source joue-t-elle en ce moment.
   *
   * Sert à décider si clear la liste doit aussi demander l'arrêt au cœur :
   * l'exiger sans condition couperait la radio quand on vide une liste de
   * fichiers qui ne jouait pas.
   */
  playing: boolean
  /**
   * Avancement du relevé des durées.
   *
   * Asynchrone côté plugin — lire l'en-tête de deux mille fichiers sur un
   * partage dépasse le plafond de 5 s du cœur — donc la page sonde le temps
   * qu'il tourne, exactement comme pour le balayage.
   */
  durations: { running: boolean; done: number; total: number }
  explore: Exploration
  /**
   * Échec de la dernière réconciliation de montage, déjà traduit.
   *
   * **Global et non porté par chaque source** : `systemctl start` réconcilie
   * toutes les racines d'un coup et ne rend qu'un seul résultat. Prétendre
   * attribuer cet échec à une source précise serait une information inventée —
   * le détail par source reste le booléen `mounted`, lui observé.
   */
  mountError: string | null
  /**
   * Points de montage dont une sonde n'est jamais revenue.
   *
   * Dits par le plugin pour que la page explique le silence : sans eux
   * l'utilisateur voit des durées qui n'arrivent pas et des états indéterminés,
   * sans aucune indication de cause.
   */
  unresponsive: string[]
}

/** Destination « stockage interne » du plugin, par opposition à un nom de root. */
export const INTERNAL = 'internal'

/**
 * Traducteur, tel que `createT` le rend. Les volets le reçoivent en propriété
 * plutôt que de le reconstruire : le catalogue arrive **après** le montage (le
 * shell monte l'IHM avec un catalogue vide le temps de le load), et un `t`
 * capturé une fois pour toutes dans un enfant figerait cet état vide.
 */
export type T = (key: string, params?: Record<string, string | number>) => string

/**
 * Émetteur d'opération, fourni par la page aux volets.
 *
 * Rend l'état **relu après l'opération**, ou `null` si le plugin a refusé (le
 * refus est alors déjà affiché par la page, verbatim). Il rend l'état plutôt
 * qu'un booléen à cause de `browse` : le résultat d'un niveau d'arborescence
 * arrive dans la relecture, et un volet qui irait le search dans sa propriété
 * `data` juste après l'`await` lirait la valeur d'**avant** le rendu du
 * parent — les propriétés ne se mettent à jour qu'au prochain cycle de Vue.
 */
export type Send = (charge: Record<string, unknown>) => Promise<Data | null>

function string_(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function number_(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0
}

function array(v: unknown): unknown[] {
  return Array.isArray(v) ? v : []
}

export function normalizeRoot(brut: unknown): Root {
  const o = (brut ?? {}) as Record<string, unknown>
  return {
    name: string_(o.name),
    // Tout ce qui n'est pas explicitement `local` est traité comme un partage :
    // le genre pilote l'affichage des champs, et se tromper dans ce sens montre
    // des champs en trop plutôt que d'en cacher.
    kind: o.kind === 'local' ? 'local' : 'smb',
    path: string_(o.path),
    host: string_(o.host),
    share: string_(o.share),
    subpath: string_(o.subpath),
    user: string_(o.user),
    domain: string_(o.domain),
    writable: o.writable === true,
    mounted: o.mounted === true,
  }
}

/**
 * Recompose un journey ou une recherche.
 *
 * Le plugin rend `dirs` et `files` comme de **simples noms**, pas des chemins :
 * un niveau est toujours lu relativement à son répertoire, et répéter le
 * préfixe sur chaque entrée gonflerait la réponse pour rien. C'est donc à la
 * page de recoller `path/nom` — et c'est ce path-là, relatif à la root, que
 * les opérations `browse`, `add_dir` et `add_file` attendent en retour.
 *
 * `results`, à l'inverse, porte déjà des chemins complets relatifs à la root :
 * une recherche traverse l'arborescence, ses trouvailles ne sont donc pas dans
 * le répertoire courant.
 */
export function normalizeBrowse(brut: unknown): Navigation {
  const o = (brut ?? {}) as Record<string, unknown>
  const base = string_(o.path)
  const joindre = (nom: string) => (base ? `${base}/${nom}` : nom)
  const entries = [
    ...array(o.dirs).map((n) => ({
      name: string_(n),
      path: joindre(string_(n)),
      dir: true,
      playlist: false,
    })),
    // Les listes avant les tracks : ce sont elles qu'on cherche quand un dossier
    // en contient, et une liste noyée sous cent fichiers ne se voit pas.
    ...array(o.playlists).map((n) => ({
      name: string_(n),
      path: joindre(string_(n)),
      dir: false,
      playlist: true,
    })),
    ...array(o.files).map((n) => ({
      name: string_(n),
      path: joindre(string_(n)),
      dir: false,
      playlist: false,
    })),
  ]
  return {
    root: string_(o.root),
    path: base,
    query: string_(o.query),
    entries,
    // La recherche ne rapporte que des fichiers audio (voir `scan::search`) :
    // aucun n'est une liste de lecture.
    results: array(o.results).map((p) => ({
      name: leaf(string_(p)),
      path: string_(p),
      dir: false,
      playlist: false,
    })),
    truncated: o.truncated === true,
    abort: o.gave_up === true,
  }
}

/**
 * Recompose l'état d'un assistant.
 *
 * Chaque champ absent vaut sa valeur vide, jamais `undefined` : pendant un
 * déploiement le plugin peut être plus ancien que la page, et un `undefined`
 * traversant un `v-for` casserait le rendu entier au lieu d'afficher une
 * section vide.
 */
export function normalizeExploration(brut: unknown): Exploration {
  if (!brut) return EMPTY_EXPLORATION
  const o = brut as Record<string, unknown>
  return {
    open: o.open === true,
    kind: o.kind === 'local' || o.kind === 'smb' ? o.kind : null,
    host: string_(o.host),
    share: string_(o.share),
    path: string_(o.path),
    shares: array(o.shares).map(string_),
    dirs: array(o.dirs).map(string_),
    audioCount: number_(o.audio_count),
    busy: o.busy === true,
    error: typeof o.error === 'string' && o.error ? o.error : null,
  }
}

/**
 * Tronque un path **par le début** pour qu'il tienne en `max` caractères.
 *
 * Par le début, et c'est tout l'intérêt : sur un path, l'information utile est
 * la fin — le dossier où l'on se trouve. Aucune propriété CSS ne sait faire
 * cela : `text-overflow` ne coupe qu'à droite, et `direction: rtl`
 * réordonnerait les segments au lieu de les tronquer.
 *
 * On retire des **segments entiers** tant que c'est trop long : couper au milieu
 * d'un nom donnerait « …ents/Ma Musique », là où « …/Ma Musique » garde un sens.
 * Le repli final ne coupe dans un nom que si ce nom dépasse à lui seul le
 * budget, faute de mieux.
 */
export function truncateStart(path: string, max = 52): string {
  if (path.length <= max) return path
  const segments = path.split('/').filter(Boolean)
  let queue = ''
  for (let i = segments.length - 1; i >= 0; i -= 1) {
    const essai = queue ? `${segments[i]}/${queue}` : segments[i]!
    // Deux caractères réservés pour le « …/ » qui annonce la coupure.
    if (essai.length + 2 > max) break
    queue = essai
  }
  if (!queue) {
    const dernier = segments[segments.length - 1] ?? path
    return `…${dernier.slice(Math.max(0, dernier.length - max + 1))}`
  }
  return `…/${queue}`
}

/** Dernier segment d'un path relatif, séparateur `/` (celui du plugin). */
export function leaf(path: string): string {
  const parts = path.split('/').filter(Boolean)
  return parts.length ? parts[parts.length - 1]! : path
}

export function normalizeData(brut: unknown): Data {
  const o = (brut ?? {}) as Record<string, unknown>
  const scan = (o.scan ?? {}) as Record<string, unknown>
  return {
    roots: array(o.roots).map(normalizeRoot),
    playlist: array(o.playlist).map((p) => {
      const e = (p ?? {}) as Record<string, unknown>
      const path = string_(e.path)
      return {
        path,
        name: string_(e.name) || leaf(path),
        duration_s: number_(e.duration_s),
        // `=== true` / `=== false` et non une coercition : c'est ce qui
        // distingue « présent » de « on ne sait pas », le second devant rester
        // `null` jusqu'à l'affichage.
        missing: e.missing === true ? true : e.missing === false ? false : null,
      }
    }),
    index: number_(o.index),
    scan: {
      running: scan.running === true,
      found: number_(scan.found),
      dir: string_(scan.dir),
      error: string_(scan.error),
    },
    saved: array(o.saved).map((s) => {
      const e = (s ?? {}) as Record<string, unknown>
      return { name: string_(e.name), where: string_(e.where) || INTERNAL }
    }),
    // Les entrées d'un m3u chargé qu'aucune règle n'a su résoudre : des chemins
    // bruts, seule chose que l'utilisateur puisse rapprocher de ses fichiers.
    unresolved: array(o.unresolved).map(string_),
    browse: normalizeBrowse(o.browse),
    volumes: array(o.volumes).map((v) => {
      const e = (v ?? {}) as Record<string, unknown>
      return { path: string_(e.path), fstype: string_(e.fstype) }
    }),
    // Faux par défaut : mieux vaut griser un assistant utilisable que d'en
    // offrir un qui échouera au clic sans dire pourquoi.
    canBrowseSmb: o.can_browse_smb === true,
    playing: o.playing === true,
    durations: (() => {
      const d = (o.durations ?? {}) as Record<string, unknown>
      return {
        running: d.running === true,
        done: number_(d.done),
        total: number_(d.total),
      }
    })(),
    explore: normalizeExploration(o.explore),
    mountError: typeof o.mount_error === 'string' && o.mount_error ? o.mount_error : null,
    unresponsive: array(o.unresponsive).map(string_).filter((s) => s !== ''),
  }
}

/**
 * Durée lisible. `0` (durée inconnue, cas d'un fichier introuvable ou d'un
 * conteneur sans en-tête) se rend par un tiret plutôt que par « 0:00 », qui
 * affirmerait une piste vide.
 */
export function formatDuration(secondes: number): string {
  if (!Number.isFinite(secondes) || secondes <= 0) return '—'
  const s = Math.round(secondes)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const r = s % 60
  const deux = (n: number) => String(n).padStart(2, '0')
  return h ? `${h}:${deux(m)}:${deux(r)}` : `${m}:${deux(r)}`
}

/** Libellé de la cible d'une root : path local, ou `//hôte/partage/sous-path`. */
export function rootTarget(r: Root): string {
  if (r.kind === 'local') return r.path
  const base = `//${r.host}/${r.share}`
  return r.subpath ? `${base}/${r.subpath}` : base
}
