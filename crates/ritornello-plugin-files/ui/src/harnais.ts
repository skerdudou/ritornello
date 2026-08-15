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
export const CATALOGUE: Record<string, string> = {
  load_error_1: 'Erreur : ',
  load_error_2: '',
  scan_progress: 'Balayage de {dir} — {found} pistes trouvées',

  roots_title: 'Racines',
  no_roots: 'Aucune racine',
  ph_root_name: 'nom',
  ph_local_path: 'chemin absolu',
  ph_host: 'serveur',
  ph_share: 'partage',
  ph_subpath: 'sous-dossier',
  ph_user: 'utilisateur',
  ph_password: 'mot de passe',
  ph_domain: 'domaine',
  kind_local: 'dossier local',
  kind_smb: 'partage réseau',
  mounted_yes: 'monté',
  mounted_no: 'non monté',
  writable_label: 'autoriser écriture',
  password_kept_hint: 'Laisser vide conserve le mot de passe enregistré.',
  btn_add_local: 'Ajouter un dossier local',
  btn_add_share: 'Déclarer un partage',
  btn_remove_root: 'Retirer la racine',
  btn_save_roots: 'Enregistrer les racines',
  btn_mount_now: 'Monter maintenant',

  browse_title: 'Parcourir',
  root_label: 'Racine',
  search_placeholder: 'chercher',
  btn_search: 'Chercher',
  no_results: 'Aucun résultat',
  btn_add_dir: 'Ajouter ce dossier',
  btn_add_file: 'Ajouter ce fichier',
  btn_expand: 'Déplier',
  btn_collapse: 'Replier',
  empty_folder: 'Dossier vide',

  playlist_title: 'Liste en cours',
  col_num: 'N°',
  col_track: 'Piste',
  col_duration: 'Durée',
  empty_playlist: 'Liste vide',
  missing_badge: 'introuvable',
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
  load_playlist_label: 'Liste à charger',
  btn_load_playlist: 'Charger',
}

export interface EtatServeur {
  roots?: unknown[]
  playlist?: unknown[]
  index?: number
  scan?: { running: boolean; found: number; dir: string }
  saved?: unknown[]
  unresolved?: unknown[]
  listing?: unknown
  search?: unknown
}

export function etat(partiel: EtatServeur = {}): Required<EtatServeur> {
  return {
    roots: [],
    playlist: [],
    index: 0,
    scan: { running: false, found: 0, dir: '' },
    saved: [],
    unresolved: [],
    listing: [],
    search: [],
    ...partiel,
  }
}

export interface Serveur {
  spy: ReturnType<typeof vi.fn>
  /** État rendu par le prochain GET. Modifiable par un test entre deux appels. */
  data: Required<EtatServeur>
  /** Quand il est non nul, tout PUT est refusé avec cette phrase, telle quelle. */
  refus: string | null
  /** Appelé avant la réponse à un PUT accepté ; sert à faire évoluer `data`. */
  surPut: (charge: Record<string, unknown>) => void
  /** Corps des PUT émis, dans l'ordre. */
  puts: () => Record<string, unknown>[]
  /** Corps des PUT portant cette opération. */
  putsDe: (op: string) => Record<string, unknown>[]
  /** URL de toutes les requêtes émises, GET comme PUT. */
  urls: () => string[]
}

/** Simulacre du plugin : un GET rend `data`, un PUT rend 204 ou un refus 422. */
export function serveur(initial: EtatServeur = {}): Serveur {
  const s: Serveur = {
    spy: vi.fn(),
    data: etat(initial),
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

/** Monte la page sur un serveur simulé et attend son premier chargement. */
export async function monter(initial: EtatServeur = {}) {
  const s = serveur(initial)
  const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, s }
}
