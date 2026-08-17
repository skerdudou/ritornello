// Formes de données échangées avec le plugin, et les quelques fonctions pures
// qui les mettent en forme.
//
// Pourquoi normaliser au lieu de consommer le JSON tel quel : le plugin
// sérialise ses structures Rust avec `skip_serializing_if = "Option::is_none"`
// (voir `roots.rs`), donc un champ vide **disparaît** du corps au lieu d'y
// figurer à vide. Une vue qui lirait `r.subpath.trim()` planterait sur la
// première racine sans sous-chemin — et un plantage dans un `computed` de Vue
// laisse la page à moitié rendue, sans message. On ramène donc tout à des
// valeurs totales une seule fois, à la frontière.

/** Genre de racine. `local` = répertoire de l'appareil, `smb` = partage réseau. */
export type GenreRacine = 'local' | 'smb'

export interface Racine {
  name: string
  kind: GenreRacine
  /** Genre `local` uniquement : chemin absolu. */
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

export interface Piste {
  path: string
  name: string
  duration_s: number
  /** Piste dont le fichier n'a pas été retrouvé : marquée, jamais masquée. */
  missing: boolean
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

export interface Enregistree {
  name: string
  /** `internal` ou le nom d'une racine. */
  where: string
}

/** Une entrée d'un niveau d'arborescence, chemin **relatif à la racine**. */
export interface Entree {
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
 * Dernier parcours ou dernière recherche, tels que le plugin les range.
 *
 * `set_data` ne rend qu'un `Ok`/`Err`, sans charge utile : le contenu voyage
 * par `get_data`, comme la recherche d'annuaire du plugin radio. Les deux
 * opérations écrivent au même endroit — une recherche efface donc le niveau
 * parcouru, et réciproquement.
 */
export interface Navigation {
  root: string
  path: string
  /** Contenu du niveau `path`, dossiers d'abord puis fichiers. */
  entrees: Entree[]
  /** Résultats de la dernière recherche. */
  resultats: Entree[]
  /** Le plugin a plafonné la recherche : il y en avait davantage. */
  tronque: boolean
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
 * qu'ouvrir une popin réinitialiserait l'arbre derrière elle.
 *
 * Aucun identifiant n'y figure. Le plugin ne les sérialise jamais : le mot de
 * passe traverse le fil une fois, à la connexion, et vit ensuite dans une
 * session en mémoire du plugin que la page ne relit pas.
 */
export interface Exploration {
  open: boolean
  kind: GenreRacine | null
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

const EXPLORATION_VIDE: Exploration = {
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

export interface Donnees {
  roots: Racine[]
  playlist: Piste[]
  index: number
  scan: Scan
  saved: Enregistree[]
  unresolved: string[]
  browse: Navigation
  volumes: Volume[]
  /** `smbclient` est-il utilisable. Faux grise l'assistant réseau, sans le retirer. */
  canBrowseSmb: boolean
  /**
   * Cette source joue-t-elle en ce moment.
   *
   * Sert à décider si vider la liste doit aussi demander l'arrêt au cœur :
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
}

/** Destination « stockage interne » du plugin, par opposition à un nom de racine. */
export const INTERNE = 'internal'

/**
 * Traducteur, tel que `createT` le rend. Les volets le reçoivent en propriété
 * plutôt que de le reconstruire : le catalogue arrive **après** le montage (le
 * shell monte l'IHM avec un catalogue vide le temps de le charger), et un `t`
 * capturé une fois pour toutes dans un enfant figerait cet état vide.
 */
export type T = (key: string, params?: Record<string, string | number>) => string

/**
 * Émetteur d'opération, fourni par la page aux volets.
 *
 * Rend l'état **relu après l'opération**, ou `null` si le plugin a refusé (le
 * refus est alors déjà affiché par la page, verbatim). Il rend l'état plutôt
 * qu'un booléen à cause de `browse` : le résultat d'un niveau d'arborescence
 * arrive dans la relecture, et un volet qui irait le chercher dans sa propriété
 * `donnees` juste après l'`await` lirait la valeur d'**avant** le rendu du
 * parent — les propriétés ne se mettent à jour qu'au prochain cycle de Vue.
 */
export type Envoyer = (charge: Record<string, unknown>) => Promise<Donnees | null>

function chaine(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function nombre(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0
}

function tableau(v: unknown): unknown[] {
  return Array.isArray(v) ? v : []
}

export function normaliserRacine(brut: unknown): Racine {
  const o = (brut ?? {}) as Record<string, unknown>
  return {
    name: chaine(o.name),
    // Tout ce qui n'est pas explicitement `local` est traité comme un partage :
    // le genre pilote l'affichage des champs, et se tromper dans ce sens montre
    // des champs en trop plutôt que d'en cacher.
    kind: o.kind === 'local' ? 'local' : 'smb',
    path: chaine(o.path),
    host: chaine(o.host),
    share: chaine(o.share),
    subpath: chaine(o.subpath),
    user: chaine(o.user),
    domain: chaine(o.domain),
    writable: o.writable === true,
    mounted: o.mounted === true,
  }
}

/**
 * Recompose un parcours ou une recherche.
 *
 * Le plugin rend `dirs` et `files` comme de **simples noms**, pas des chemins :
 * un niveau est toujours lu relativement à son répertoire, et répéter le
 * préfixe sur chaque entrée gonflerait la réponse pour rien. C'est donc à la
 * page de recoller `path/nom` — et c'est ce chemin-là, relatif à la racine, que
 * les opérations `browse`, `add_dir` et `add_file` attendent en retour.
 *
 * `results`, à l'inverse, porte déjà des chemins complets relatifs à la racine :
 * une recherche traverse l'arborescence, ses trouvailles ne sont donc pas dans
 * le répertoire courant.
 */
export function normaliserBrowse(brut: unknown): Navigation {
  const o = (brut ?? {}) as Record<string, unknown>
  const base = chaine(o.path)
  const joindre = (nom: string) => (base ? `${base}/${nom}` : nom)
  const entrees = [
    ...tableau(o.dirs).map((n) => ({
      name: chaine(n),
      path: joindre(chaine(n)),
      dir: true,
      playlist: false,
    })),
    // Les listes avant les pistes : ce sont elles qu'on cherche quand un dossier
    // en contient, et une liste noyée sous cent fichiers ne se voit pas.
    ...tableau(o.playlists).map((n) => ({
      name: chaine(n),
      path: joindre(chaine(n)),
      dir: false,
      playlist: true,
    })),
    ...tableau(o.files).map((n) => ({
      name: chaine(n),
      path: joindre(chaine(n)),
      dir: false,
      playlist: false,
    })),
  ]
  return {
    root: chaine(o.root),
    path: base,
    entrees,
    // La recherche ne rapporte que des fichiers audio (voir `scan::search`) :
    // aucun n'est une liste de lecture.
    resultats: tableau(o.results).map((p) => ({
      name: feuille(chaine(p)),
      path: chaine(p),
      dir: false,
      playlist: false,
    })),
    tronque: o.truncated === true,
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
export function normaliserExploration(brut: unknown): Exploration {
  if (!brut) return EXPLORATION_VIDE
  const o = brut as Record<string, unknown>
  return {
    open: o.open === true,
    kind: o.kind === 'local' || o.kind === 'smb' ? o.kind : null,
    host: chaine(o.host),
    share: chaine(o.share),
    path: chaine(o.path),
    shares: tableau(o.shares).map(chaine),
    dirs: tableau(o.dirs).map(chaine),
    audioCount: nombre(o.audio_count),
    busy: o.busy === true,
    error: typeof o.error === 'string' && o.error ? o.error : null,
  }
}

/**
 * Tronque un chemin **par le début** pour qu'il tienne en `max` caractères.
 *
 * Par le début, et c'est tout l'intérêt : sur un chemin, l'information utile est
 * la fin — le dossier où l'on se trouve. Aucune propriété CSS ne sait faire
 * cela : `text-overflow` ne coupe qu'à droite, et `direction: rtl`
 * réordonnerait les segments au lieu de les tronquer.
 *
 * On retire des **segments entiers** tant que c'est trop long : couper au milieu
 * d'un nom donnerait « …ents/Ma Musique », là où « …/Ma Musique » garde un sens.
 * Le repli final ne coupe dans un nom que si ce nom dépasse à lui seul le
 * budget, faute de mieux.
 */
export function tronquerDebut(chemin: string, max = 52): string {
  if (chemin.length <= max) return chemin
  const segments = chemin.split('/').filter(Boolean)
  let queue = ''
  for (let i = segments.length - 1; i >= 0; i -= 1) {
    const essai = queue ? `${segments[i]}/${queue}` : segments[i]!
    // Deux caractères réservés pour le « …/ » qui annonce la coupure.
    if (essai.length + 2 > max) break
    queue = essai
  }
  if (!queue) {
    const dernier = segments[segments.length - 1] ?? chemin
    return `…${dernier.slice(Math.max(0, dernier.length - max + 1))}`
  }
  return `…/${queue}`
}

/** Dernier segment d'un chemin relatif, séparateur `/` (celui du plugin). */
export function feuille(chemin: string): string {
  const parts = chemin.split('/').filter(Boolean)
  return parts.length ? parts[parts.length - 1]! : chemin
}

export function normaliserDonnees(brut: unknown): Donnees {
  const o = (brut ?? {}) as Record<string, unknown>
  const scan = (o.scan ?? {}) as Record<string, unknown>
  return {
    roots: tableau(o.roots).map(normaliserRacine),
    playlist: tableau(o.playlist).map((p) => {
      const e = (p ?? {}) as Record<string, unknown>
      const path = chaine(e.path)
      return {
        path,
        name: chaine(e.name) || feuille(path),
        duration_s: nombre(e.duration_s),
        missing: e.missing === true,
      }
    }),
    index: nombre(o.index),
    scan: {
      running: scan.running === true,
      found: nombre(scan.found),
      dir: chaine(scan.dir),
      error: chaine(scan.error),
    },
    saved: tableau(o.saved).map((s) => {
      const e = (s ?? {}) as Record<string, unknown>
      return { name: chaine(e.name), where: chaine(e.where) || INTERNE }
    }),
    // Les entrées d'un m3u chargé qu'aucune règle n'a su résoudre : des chemins
    // bruts, seule chose que l'utilisateur puisse rapprocher de ses fichiers.
    unresolved: tableau(o.unresolved).map(chaine),
    browse: normaliserBrowse(o.browse),
    volumes: tableau(o.volumes).map((v) => {
      const e = (v ?? {}) as Record<string, unknown>
      return { path: chaine(e.path), fstype: chaine(e.fstype) }
    }),
    // Faux par défaut : mieux vaut griser un assistant utilisable que d'en
    // offrir un qui échouera au clic sans dire pourquoi.
    canBrowseSmb: o.can_browse_smb === true,
    playing: o.playing === true,
    durations: (() => {
      const d = (o.durations ?? {}) as Record<string, unknown>
      return {
        running: d.running === true,
        done: nombre(d.done),
        total: nombre(d.total),
      }
    })(),
    explore: normaliserExploration(o.explore),
    mountError: typeof o.mount_error === 'string' && o.mount_error ? o.mount_error : null,
  }
}

/**
 * Durée lisible. `0` (durée inconnue, cas d'un fichier introuvable ou d'un
 * conteneur sans en-tête) se rend par un tiret plutôt que par « 0:00 », qui
 * affirmerait une piste vide.
 */
export function formaterDuree(secondes: number): string {
  if (!Number.isFinite(secondes) || secondes <= 0) return '—'
  const s = Math.round(secondes)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const r = s % 60
  const deux = (n: number) => String(n).padStart(2, '0')
  return h ? `${h}:${deux(m)}:${deux(r)}` : `${m}:${deux(r)}`
}

/** Libellé de la cible d'une racine : chemin local, ou `//hôte/partage/sous-chemin`. */
export function cibleRacine(r: Racine): string {
  if (r.kind === 'local') return r.path
  const base = `//${r.host}/${r.share}`
  return r.subpath ? `${base}/${r.subpath}` : base
}
