// Harnais partagé des tests de ce module.
//
// Il vit dans `src/` et non dans un `*.test.ts` pour être importable par
// plusieurs fichiers de test ; il n'est atteignable depuis aucun import de
// `src/index.ts`, donc il n'entre jamais dans le paquet construit.

import { flushPromises, mount } from '@vue/test-utils'
import { vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'

/**
 * Préfixe volontairement différent de `/plugins/files/` : le nom sous lequel un
 * plugin est servi vient de `plugins.toml`, donc du déploiement. Un module qui
 * reconstruirait son propre nom passerait un test posé sur le nom attendu.
 */
export const BASE = '/plugins/mediatheque/'

/**
 * Catalogue de test. Les valeurs sont en français et **distinctes** les unes
 * des autres : un test qui cherche « Monter » dans le texte de la page ne doit
 * pas pouvoir réussir grâce à un autre libellé.
 */
export const CATALOG: Record<string, string> = {
  load_error_1: 'Erreur : ',
  load_error_2: '',
  scan_progress: 'Balayage de {dir} — {found} tracks trouvées',

  ph_host: 'server',
  ph_share: 'partage',
  ph_subpath: 'sous-dossier',
  ph_user: 'utilisateur',
  ph_password: 'mot de passe',
  ph_domain: 'domaine Windows (optionnel)',
  kind_local: 'dossier local',
  kind_smb: 'partage réseau',
  mounted_yes: 'monté',
  mounted_no: 'non monté',
  writable_label: 'autoriser écriture',
  btn_add_share: 'Déclarer un partage',

  sources_title: 'Sources',
  no_sources: 'Aucune source déclarée',
  btn_add_device: 'Ajouter un dossier de l’appareil',
  btn_add_to_playlist: 'Ajouter à la liste',
  btn_load_m3u: 'Charger cette liste',
  btn_remove_source: 'Retirer cette source',
  btn_retry_mount: 'Réessayer le montage',
  mount_error_title: 'Le dernier montage a échoué :',
  unresponsive_title: 'Ces points de montage ne répondent pas :',
  unresponsive_hint: 'Informations incomplètes jusqu’à leur retour.',
  missing_unknown: 'indéterminé',

  dlg_device_title: 'Choisir un dossier de l’appareil',
  dlg_device_desc: 'Choisissez un volume puis descendez.',
  volumes_label: 'Volume',
  no_volumes: 'Aucun volume exploitable',
  audio_here: '{count} fichiers audio ici',
  btn_choose_folder: 'Choisir ce dossier',
  btn_up: 'Remonter d’un niveau',
  ph_manual_path: 'ou saisir un path absolu',
  btn_go: 'Ouvrir',
  btn_cancel: 'Annuler',

  dlg_share_title: 'Choisir un partage réseau',
  dlg_share_desc: 'Indiquez une adresse puis connectez-vous.',
  btn_connect: 'Se connect',
  connecting: 'Connexion en cours',
  shares_label: 'Partage',
  btn_manual: 'Saisir à la main',
  btn_assistant: 'Revenir à l’assistant',
  smb_unavailable: 'Le paquet smbclient manque pour parcourir un partage.',

  browse_title: 'Parcourir',
  root_label: 'Root',
  search_placeholder: 'search',
  btn_search: 'Chercher',
  no_results: 'Aucun résultat',
  search_truncated: 'Seuls les {count} premiers sont affichés : affinez la recherche.',
  search_gave_up:
    'la recherche s’est arrêtée avant d’avoir parcouru tout ce dossier : ouvrez un sous-dossier et cherchez-y.',
  empty_folder: 'Dossier vide',
  btn_add_current_folder: 'Ajouter ce dossier',
  search_scope: 'La recherche porte sur {path}',

  playlist_title: 'Liste en cours',
  col_num: 'N°',
  col_track: 'Track',
  col_duration: 'Durée',
  empty_playlist: 'Liste vide',
  missing_badge: 'introuvable',
  reorder_hint: 'Glisser pour réordonner',
  duration_progress: 'Lecture des durées ({done} sur {total})',
  btn_move_up: 'Monter la piste',
  btn_move_down: 'Descendre la piste',
  btn_remove_track: 'Retirer la piste',
  btn_clear: 'Vider la liste',
  page_range: '{from}–{to} sur {total}',
  unresolved_title: '{count} entrées non retrouvées',
  ph_playlist_name: 'nom de la liste',
  dest_label: 'Destination',
  dest_internal: 'stockage interne',
  btn_save_playlist: 'Enregistrer la liste',
  no_saved: 'Aucune liste enregistrée',
  load_playlist_label: 'Liste à load',
  btn_load_playlist: 'Charger',
}

/** Contenu du champ `browse`, où le plugin range journey **et** recherche. */
export interface Navigate {
  root: string
  path: string
  /** Vide pour un journey, le motif pour une recherche. */
  query?: string
  /** Noms nus, pas des chemins : c'est ce que `scan::list_dir` rend. */
  dirs: string[]
  files: string[]
  /** Fichiers `.m3u`/`.m3u8` du niveau : ils se chargent, ils ne s'ajoutent pas. */
  playlists?: string[]
  /** Chemins relatifs à la root, rendus par `search`. */
  results: string[]
  truncated?: boolean
  /** Le journey a été interrompu avant d'avoir tout vu, distinct de `truncated`. */
  gave_up?: boolean
}

export interface ServerState {
  roots?: unknown[]
  playlist?: unknown[]
  index?: number
  scan?: { running: boolean; found: number; dir: string; error?: string }
  saved?: unknown[]
  unresolved?: string[]
  browse?: Navigate
  volumes?: { path: string; fstype: string }[]
  can_browse_smb?: boolean
  playing?: boolean
  durations?: { running: boolean; done: number; total: number }
  explore?: Explore
  mount_error?: string | null
  unresponsive?: string[]
}

/**
 * Champ `explore` du plugin : l'assistant en cours.
 *
 * Emplacement distinct de `browse`, comme côté plugin : la popin et le volet
 * Parcourir sont deux curseurs indépendants.
 */
export interface Explore {
  open?: boolean
  kind?: 'local' | 'smb' | null
  host?: string
  share?: string
  path?: string
  shares?: string[]
  dirs?: string[]
  audio_count?: number
  busy?: boolean
  error?: string | null
}

/** Assistant fermé : l'état de repos, celui d'une page qui vient de load. */
export const EXPLORE_CLOSED: Explore = {
  open: false,
  kind: null,
  host: '',
  share: '',
  path: '',
  shares: [],
  dirs: [],
  audio_count: 0,
  busy: false,
  error: null,
}

export function state(partiel: ServerState = {}): Required<ServerState> {
  return {
    roots: [],
    playlist: [],
    index: 0,
    scan: { running: false, found: 0, dir: '' },
    saved: [],
    unresolved: [],
    browse: { root: '', path: '', query: '', dirs: [], files: [], results: [] },
    volumes: [],
    // Faux par défaut, comme le plugin quand `smbclient` manque : c'est l'état
    // qu'un test doit déclarer explicitement pour offrir l'assistant réseau.
    can_browse_smb: false,
    playing: false,
    durations: { running: false, done: 0, total: 0 },
    explore: EXPLORE_CLOSED,
    mount_error: null,
    unresponsive: [],
    ...partiel,
  }
}

export interface Server {
  spy: ReturnType<typeof vi.fn>
  /** État rendu par le prochain GET. Modifiable par un test entre deux appels. */
  data: Required<ServerState>
  /** Quand il est non nul, tout PUT est refusé avec cette phrase, telle quelle. */
  refus: string | null
  /** Appelé avant la réponse à un PUT accepté ; sert à faire évoluer `data`. */
  surPut: (charge: Record<string, unknown>) => void
  /** Corps des PUT émis, dans l'order. */
  puts: () => Record<string, unknown>[]
  /** Corps des PUT portant cette opération. */
  putsDe: (op: string) => Record<string, unknown>[]
  /** URL de toutes les requêtes émises, GET comme PUT. */
  urls: () => string[]
}

/** Simulacre du plugin : un GET rend `data`, un PUT rend 204 ou un refus 422. */
export function server(initial: ServerState = {}): Server {
  const s: Server = {
    spy: vi.fn(),
    data: state(initial),
    refus: null,
    surPut: () => {},
    puts: () => s.spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)) as Record<string, unknown>),
    putsDe: (op) => s.puts().filter((b) => b.op === op),
    urls: () => s.spy.mock.calls.map((c) => String(c[0])),
  }
  s.spy.mockImplementation(async (_url: string, init?: RequestInit) => {
    if (init?.method === 'PUT' || init?.method === 'POST') {
      if (s.refus !== null) {
        return new Response(JSON.stringify({ error: s.refus }), { status: 422 })
      }
      s.surPut(JSON.parse(String(init.body)) as Record<string, unknown>)
      return new Response(null, { status: 204 })
    }
    return new Response(JSON.stringify(s.data), { status: 200 })
  })
  vi.stubGlobal('fetch', s.spy)
  return s
}

/**
 * Monte la page sur un server simulé et attend son premier chargement.
 *
 * `attachTo: document.body` n'est pas décoratif : le `Dialog` du kit rend son
 * contenu à travers un `DialogPortal`, donc **hors** de l'arbre du composant.
 * Sans rattachement, la popin n'est pas rendue du tout ; avec, elle atterrit
 * dans `document.body` — et reste invisible à `wrapper.find()`. Voir
 * `inPopover` ci-dessous, qui est le seul moyen correct de l'atteindre.
 */
export async function mountAdmin(initial: ServerState = {}) {
  const s = server(initial)
  const w = mount(FilesAdmin, {
    props: { catalog: CATALOG, base: BASE },
    attachTo: document.body,
  })
  await flushPromises()
  return { w, s }
}

/**
 * Un élément de la popin ouverte.
 *
 * `wrapper.find()` ne le trouvera **jamais** : le contenu d'un `Dialog` vit
 * dans un portail vers `document.body`, en dehors de l'arbre monté. Mesuré sur
 * ce dépôt — un test qui interroge le wrapper échoue avec « élément absent »
 * alors que la popin est bien à l'écran, ce qui envoie search un défaut là
 * où il n'y en a pas.
 */
export function inPopover(selecteur: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(selecteur)
}

/** Clique dans la popin, puis laisse Vue et les promesses se dérouler. */
export async function clickPopover(selecteur: string): Promise<void> {
  const el = inPopover(selecteur)
  if (!el) throw new Error(`aucun élément « ${selecteur} » dans la popin`)
  el.click()
  await flushPromises()
}

/** Saisit dans un champ de la popin, en notifiant Vue du changement. */
export async function typeInPopover(selecteur: string, valeur: string): Promise<void> {
  const el = inPopover(selecteur) as HTMLInputElement | null
  if (!el) throw new Error(`aucun champ « ${selecteur} » dans la popin`)
  el.value = valeur
  el.dispatchEvent(new Event('input', { bubbles: true }))
  await flushPromises()
}

/**
 * Vide `document.body` entre deux tests.
 *
 * Les portails n'y sont pas nettoyés par le démontage du wrapper : sans cet
 * appel, la popin d'un test précédent resterait dans le document et le test
 * suivant interrogerait le mauvais panneau.
 */
export function cleanupPopovers(): void {
  document.body.innerHTML = ''
}
