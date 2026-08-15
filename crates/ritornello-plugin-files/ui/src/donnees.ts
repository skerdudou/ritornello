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
}

export interface Enregistree {
  name: string
  /** `internal` ou le nom d'une racine. */
  where: string
}

/** Une entrée d'un niveau d'arborescence. */
export interface Entree {
  name: string
  path: string
  dir: boolean
}

export interface Donnees {
  roots: Racine[]
  playlist: Piste[]
  index: number
  scan: Scan
  saved: Enregistree[]
  unresolved: string[]
  /** Dernier niveau parcouru, rendu par le plugin après une opération `browse`. */
  listing: Entree[]
  /** Derniers résultats de recherche, après une opération `search`. */
  search: Entree[]
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
 * Entrées d'un niveau, quelle que soit la forme rendue.
 *
 * Trois formes sont acceptées — un tableau plat d'entrées portant `dir`, un
 * objet `{entries}`, ou un objet `{dirs, files}` — parce que ce champ n'est pas
 * décrit par le contrat écrit du plugin, seulement par son implémentation. Un
 * lecteur tolérant coûte dix lignes ; une page qui affiche un dossier vide
 * parce que le serveur a nommé son champ `dirs` au lieu d'`entries` coûte une
 * séance de débogage à travers le socket d'admin.
 */
export function normaliserEntrees(brut: unknown): Entree[] {
  if (brut == null) return []
  if (Array.isArray(brut)) return brut.map((e) => entree(e, undefined))
  const o = brut as Record<string, unknown>
  if (Array.isArray(o.entries)) return o.entries.map((e) => entree(e, undefined))
  const dossiers = tableau(o.dirs).map((e) => entree(e, true))
  const fichiers = tableau(o.files).map((e) => entree(e, false))
  return [...dossiers, ...fichiers]
}

function entree(brut: unknown, dossier: boolean | undefined): Entree {
  // Une entrée réduite à une chaîne est acceptée : c'est le chemin, et le nom
  // s'en déduit. Le plugin n'a alors rien à inventer pour les listes plates.
  if (typeof brut === 'string') {
    return { name: feuille(brut), path: brut, dir: dossier ?? false }
  }
  const o = (brut ?? {}) as Record<string, unknown>
  const path = chaine(o.path)
  return {
    name: chaine(o.name) || feuille(path),
    path,
    dir: dossier ?? o.dir === true,
  }
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
    scan: { running: scan.running === true, found: nombre(scan.found), dir: chaine(scan.dir) },
    saved: tableau(o.saved).map((s) => {
      const e = (s ?? {}) as Record<string, unknown>
      return { name: chaine(e.name), where: chaine(e.where) || INTERNE }
    }),
    // Les entrées non résolues d'un m3u : le plugin peut aussi bien rendre des
    // chemins nus que des objets. Les deux se ramènent au chemin, seule chose
    // que l'utilisateur peut rapprocher de son fichier.
    unresolved: tableau(o.unresolved).map((u) =>
      typeof u === 'string' ? u : chaine((u as Record<string, unknown>)?.path),
    ),
    listing: normaliserEntrees(o.listing),
    search: normaliserEntrees(o.search),
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
