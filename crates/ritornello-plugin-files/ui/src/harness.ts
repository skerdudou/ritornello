// Shared harness of this module's tests.
//
// It lives in `src/` and not in a `*.test.ts` so that several test files can
// import it; it is reachable from no import of `src/index.ts`, so it never
// enters the built package.

import { flushPromises, mount } from '@vue/test-utils'
import { vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'

/**
 * Prefix deliberately different from `/plugins/files/`: the name under which a
 * plugin is served comes from `plugins.toml`, hence from the deployment. A
 * module that rebuilt its own name would pass a test written against the
 * expected name.
 */
export const BASE = '/plugins/mediatheque/'

/**
 * Test catalog. The values are made up and **distinct** from one another: a
 * test that looks for "Mount" in the page text must not be able to succeed
 * thanks to another label.
 */
export const CATALOG: Record<string, string> = {
  // Inherited from the common vocabulary (`common_en.toml`) by every plugin
  // catalog, exactly as the real merged catalog delivers it.
  loading: 'Loading…',
  load_error_1: 'Error: ',
  load_error_2: '',
  scan_progress: 'Scanning {dir} — {found} tracks found so far',

  ph_host: 'server',
  ph_share: 'share name',
  ph_subpath: 'subfolder',
  ph_user: 'username',
  ph_password: 'passphrase',
  ph_domain: 'Windows domain (optional)',
  kind_local: 'local folder',
  kind_smb: 'network share',
  mounted_yes: 'mounted',
  mounted_no: 'not mounted',
  writable_label: 'allow writes',
  btn_add_share: 'Declare a share',

  sources_title: 'Sources',
  no_sources: 'No source declared',
  btn_add_device: 'Add a device folder',
  btn_add_to_playlist: 'Add to the queue',
  btn_load_m3u: 'Load this playlist',
  btn_remove_source: 'Remove this source',
  btn_retry_mount: 'Retry the mount',
  mount_error_title: 'The last mount attempt failed:',
  unresponsive_title: 'These mount points are not responding:',
  unresponsive_hint: 'Information stays incomplete until they respond again.',
  missing_unknown: 'unclear',

  dlg_device_title: 'Pick a device folder',
  dlg_device_desc: 'Choose a volume then browse down.',
  volumes_label: 'Volume',
  no_volumes: 'No usable volume',
  audio_here: '{count} audio files here',
  btn_choose_folder: 'Choose this folder',
  btn_up: 'Go up one level',
  ph_manual_path: 'or type an absolute path',
  btn_go: 'Open',
  btn_cancel: 'Cancel',

  dlg_share_title: 'Pick a network share',
  dlg_share_desc: 'Enter an address then connect.',
  btn_connect: 'Connect',
  connecting: 'Connecting',
  shares_label: 'Share',
  btn_manual: 'Type it by hand',
  btn_assistant: 'Back to the wizard',
  smb_unavailable: 'The smbclient package is missing to browse a share.',

  browse_title: 'Browse',
  root_label: 'Root',
  search_placeholder: 'search',
  btn_search: 'Search',
  no_results: 'No result',
  search_truncated: 'Only the first {count} are shown: narrow your search.',
  search_gave_up:
    'the search stopped before covering this whole folder: open a subfolder and search there instead.',
  empty_folder: 'Empty folder',
  btn_add_current_folder: 'Add this folder',
  search_scope: 'Searching within {path}',

  playlist_title: 'Current queue',
  col_num: 'No.',
  col_track: 'Track',
  col_duration: 'Length',
  empty_playlist: 'Empty queue',
  missing_badge: 'not found',
  reorder_hint: 'Drag to reorder',
  duration_progress: 'Reading lengths ({done} of {total})',
  btn_move_up: 'Move track up',
  btn_move_down: 'Move track down',
  btn_remove_track: 'Remove track',
  btn_clear: 'Clear the queue',
  page_range: '{from}–{to} of {total}',
  unresolved_title: '{count} entries could not be found',
  ph_playlist_name: 'playlist name',
  dest_label: 'Destination',
  dest_internal: 'internal storage',
  btn_save_playlist: 'Save the queue',
  no_saved: 'No saved playlist',
  load_playlist_label: 'Playlist to load',
  btn_load_playlist: 'Load',
}

/** Content of the `browse` field, where the plugin stores browse **and** search. */
export interface Navigate {
  root: string
  path: string
  /** Empty for a browse, the pattern for a search. */
  query?: string
  /** Bare names, not paths: this is what `scan::list_dir` returns. */
  dirs: string[]
  files: string[]
  /** `.m3u`/`.m3u8` files of the level: they load, they are not added. */
  playlists?: string[]
  /** Paths relative to the root, returned by `search`. */
  results: string[]
  truncated?: boolean
  /** The walk was interrupted before it had seen everything, distinct from `truncated`. */
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
 * The plugin's `explore` field: the wizard in progress.
 *
 * A location distinct from `browse`, as on the plugin side: the dialog and the
 * Browse pane are two independent cursors.
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

/** Wizard closed: the resting state, that of a page that has just loaded. */
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

export function state(partial: ServerState = {}): Required<ServerState> {
  return {
    roots: [],
    playlist: [],
    index: 0,
    scan: { running: false, found: 0, dir: '' },
    saved: [],
    unresolved: [],
    browse: { root: '', path: '', query: '', dirs: [], files: [], results: [] },
    volumes: [],
    // False by default, like the plugin when `smbclient` is missing: this is
    // the state a test must declare explicitly to offer the network wizard.
    can_browse_smb: false,
    playing: false,
    durations: { running: false, done: 0, total: 0 },
    explore: EXPLORE_CLOSED,
    mount_error: null,
    unresponsive: [],
    ...partial,
  }
}

export interface Server {
  spy: ReturnType<typeof vi.fn>
  /** State returned by the next GET. Modifiable by a test between two calls. */
  data: Required<ServerState>
  /** When non-null, every PUT is refused with this sentence, as is. */
  refusal: string | null
  /** Called before the response to an accepted PUT; used to evolve `data`. */
  onPut: (payload: Record<string, unknown>) => void
  /** Bodies of the emitted PUTs, in order. */
  puts: () => Record<string, unknown>[]
  /** Bodies of the PUTs carrying this operation. */
  putsOf: (op: string) => Record<string, unknown>[]
  /** URL of every emitted request, GET as well as PUT. */
  urls: () => string[]
}

/** Stand-in for the plugin: a GET returns `data`, a PUT returns 204 or a 422 refusal. */
export function server(initial: ServerState = {}): Server {
  const s: Server = {
    spy: vi.fn(),
    data: state(initial),
    refusal: null,
    onPut: () => {},
    puts: () => s.spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)) as Record<string, unknown>),
    putsOf: (op) => s.puts().filter((b) => b.op === op),
    urls: () => s.spy.mock.calls.map((c) => String(c[0])),
  }
  s.spy.mockImplementation(async (_url: string, init?: RequestInit) => {
    if (init?.method === 'PUT' || init?.method === 'POST') {
      if (s.refusal !== null) {
        return new Response(JSON.stringify({ error: s.refusal }), { status: 422 })
      }
      s.onPut(JSON.parse(String(init.body)) as Record<string, unknown>)
      return new Response(null, { status: 204 })
    }
    return new Response(JSON.stringify(s.data), { status: 200 })
  })
  vi.stubGlobal('fetch', s.spy)
  return s
}

/**
 * Mounts the page on a simulated server and waits for its first load.
 *
 * `attachTo: document.body` is not decorative: the kit's `Dialog` renders its
 * content through a `DialogPortal`, hence **outside** the component tree.
 * Without attaching, the dialog is not rendered at all; with it, it lands in
 * `document.body` — and stays invisible to `wrapper.find()`. See `inPopover`
 * below, which is the only correct way to reach it.
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
 * An element of the open dialog.
 *
 * `wrapper.find()` will **never** find it: the content of a `Dialog` lives in a
 * portal to `document.body`, outside the mounted tree. Measured on this repo —
 * a test that queries the wrapper fails with "element absent" while the dialog
 * is indeed on screen, which sends one looking for a defect where there is
 * none.
 */
export function inPopover(selector: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(selector)
}

/** Clicks inside the dialog, then lets Vue and the promises unfold. */
export async function clickPopover(selector: string): Promise<void> {
  const el = inPopover(selector)
  if (!el) throw new Error(`no element "${selector}" in the dialog`)
  el.click()
  await flushPromises()
}

/** Types into a field of the dialog, notifying Vue of the change. */
export async function typeInPopover(selector: string, value: string): Promise<void> {
  const el = inPopover(selector) as HTMLInputElement | null
  if (!el) throw new Error(`no field "${selector}" in the dialog`)
  el.value = value
  el.dispatchEvent(new Event('input', { bubbles: true }))
  await flushPromises()
}

/**
 * Empties `document.body` between two tests.
 *
 * The portals are not cleaned up there by unmounting the wrapper: without this
 * call, the dialog of a previous test would remain in the document and the next
 * test would query the wrong panel.
 */
export function cleanupPopovers(): void {
  document.body.innerHTML = ''
}
