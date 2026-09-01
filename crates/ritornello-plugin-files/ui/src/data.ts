// Shapes of the data exchanged with the plugin, and the few pure functions
// that put them into shape.
//
// Why normalize instead of consuming the JSON as is: the plugin serializes
// its Rust structures with `skip_serializing_if = "Option::is_none"` (see
// `roots.rs`), so an empty field **disappears** from the body instead of
// appearing empty. A view that read `r.subpath.trim()` would crash on the
// first root without a subpath — and a crash inside a Vue `computed` leaves
// the page half rendered, with no message. So everything is brought back to
// total values once, at the boundary.

/** Kind of root. `local` = a directory of the device, `smb` = a network share. */
export type RootKind = 'local' | 'smb'

export interface Root {
  name: string
  kind: RootKind
  /** Kind `local` only: absolute path. */
  path: string
  host: string
  share: string
  subpath: string
  user: string
  domain: string
  writable: boolean
  /** Observed state of the mount, rendered by the plugin; never entered by the page. */
  mounted: boolean
}

export interface Track {
  path: string
  name: string
  duration_s: number
  /**
   * Track whose file was not found: marked, never hidden.
   *
   * Three states, not two. `null` means **undetermined**: the mount point of
   * the track was not answering when the plugin looked. Showing "not found"
   * in that case would blame the file for a failure that belongs to the share,
   * and would send the user looking for the defect in the wrong place.
   */
  missing: boolean | null
}

export interface Scan {
  running: boolean
  found: number
  dir: string
  /**
   * Refusal or incident of the **last** scan, already translated by the plugin.
   *
   * It survives the end of the scan, and that is deliberate on the plugin side:
   * `add_dir` returns long before the recursive walk finishes, so this is the
   * only place where the page can learn that an addition failed. The empty
   * string means "nothing to report".
   */
  error: string
}

export interface Saved {
  name: string
  /** `internal` or the name of a root. */
  where: string
}

/** One entry of a tree level, path **relative to the root**. */
export interface Entry {
  name: string
  path: string
  dir: boolean
  /**
   * Playlist file (`.m3u`, `.m3u8`).
   *
   * Exclusive with `dir`. It carries a different action from the others: a
   * playlist **loads** — it replaces the current playlist — whereas a folder or
   * a track is added to it. Confusing them would add a text file that mpv would
   * try to play.
   */
  playlist: boolean
}

/**
 * Last browse or last search, as the plugin stores them.
 *
 * `set_data` only returns an `Ok`/`Err`, without payload: the content travels
 * through `get_data`, like the directory search of the radio plugin. Both
 * operations write to the same place — so a search erases the browsed level,
 * and vice versa.
 */
export interface Navigation {
  root: string
  path: string
  /**
   * Pattern of the last search, empty for a browse.
   *
   * What the page does with it: tell the answer to ITS browse apart from the
   * answer to a search over the same folder — both are stored in the same place
   * on the plugin side.
   */
  query: string
  /** Content of level `path`, folders first then files. */
  entries: Entry[]
  /** Results of the last search. */
  results: Entry[]
  /** The plugin capped the search: there were more. */
  truncated: boolean
  /**
   * The walk was interrupted before it had seen everything, distinct from `truncated`.
   *
   * Two causes, two pieces of advice: `truncated` invites to refine the
   * pattern, `abort` invites to go down into a subfolder. Confusing them made
   * the page show "No result" — hence "this file does not exist" — for a search
   * that had simply given up before reaching it.
   */
  abort: boolean
}

/** A mounted volume of the device, as the plugin reads it from `/proc/mounts`. */
export interface Volume {
  path: string
  fstype: string
}

/**
 * The declaration wizard in progress.
 *
 * A location **distinct** from `browse`: the dialog and the Browse pane are two
 * independent cursors, and making them share a location would make opening a
 * dialog reset the tree behind it.
 *
 * No credential appears here. The plugin never serializes them: the password
 * crosses the wire once, at connection time, and then lives in an in-memory
 * session of the plugin that the page does not read back.
 */
export interface Exploration {
  open: boolean
  kind: RootKind | null
  host: string
  share: string
  /** Absolute path for a volume, relative to the share for a share. */
  path: string
  shares: string[]
  dirs: string[]
  /** Audio files of the open level: this is what says we are in the right place. */
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
  /** Is `smbclient` usable. False greys out the network wizard, without removing it. */
  canBrowseSmb: boolean
  /**
   * Is this source playing right now.
   *
   * Used to decide whether clearing the playlist must also ask the core to
   * stop: requiring it unconditionally would cut the radio when one empties a
   * file playlist that was not playing.
   */
  playing: boolean
  /**
   * Progress of the duration survey.
   *
   * Asynchronous on the plugin side — reading the header of two thousand files
   * on a share exceeds the core's 5 s cap — so the page probes as long as it
   * runs, exactly as for the scan.
   */
  durations: { running: boolean; done: number; total: number }
  explore: Exploration
  /**
   * Failure of the last mount reconciliation, already translated.
   *
   * **Global and not carried by each source**: `systemctl start` reconciles all
   * roots at once and returns a single result. Pretending to attribute this
   * failure to a specific source would be invented information — the
   * per-source detail remains the `mounted` boolean, which is observed.
   */
  mountError: string | null
  /**
   * Mount points from which a probe never came back.
   *
   * Told by the plugin so that the page can explain the silence: without them
   * the user sees durations that never arrive and undetermined states, with no
   * indication of the cause.
   */
  unresponsive: string[]
}

/** The plugin's "internal storage" destination, as opposed to a root name. */
export const INTERNAL = 'internal'

/**
 * Translator, as `createT` returns it. The panes receive it as a prop rather
 * than rebuilding it: the catalog arrives **after** mounting (the shell mounts
 * the UI with an empty catalog while loading it), and a `t` captured once and
 * for all in a child would freeze that empty state.
 */
export type T = (key: string, params?: Record<string, string | number>) => string

/**
 * Operation emitter, provided by the page to the panes.
 *
 * Returns the state **re-read after the operation**, or `null` if the plugin
 * refused (the refusal is then already displayed by the page, verbatim). It
 * returns the state rather than a boolean because of `browse`: the result of a
 * tree level arrives in the re-read, and a pane that went looking for it in its
 * `data` prop right after the `await` would read the value from **before** the
 * parent's render — props only update on the next Vue cycle.
 */
export type Send = (payload: Record<string, unknown>) => Promise<Data | null>

function string_(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function number_(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0
}

function array(v: unknown): unknown[] {
  return Array.isArray(v) ? v : []
}

export function normalizeRoot(raw: unknown): Root {
  const o = (raw ?? {}) as Record<string, unknown>
  return {
    name: string_(o.name),
    // Anything that is not explicitly `local` is treated as a share: the kind
    // drives which fields are displayed, and erring in this direction shows
    // extra fields rather than hiding some.
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
 * Recomposes a browse or a search.
 *
 * The plugin returns `dirs` and `files` as **plain names**, not paths: a level
 * is always read relative to its directory, and repeating the prefix on every
 * entry would inflate the response for nothing. So it is up to the page to
 * glue `path/name` back together — and it is that path, relative to the root,
 * that the `browse`, `add_dir` and `add_file` operations expect in return.
 *
 * `results`, conversely, already carries full paths relative to the root: a
 * search traverses the tree, so its findings are not in the current directory.
 */
export function normalizeBrowse(raw: unknown): Navigation {
  const o = (raw ?? {}) as Record<string, unknown>
  const base = string_(o.path)
  const join = (name: string) => (base ? `${base}/${name}` : name)
  const entries = [
    ...array(o.dirs).map((n) => ({
      name: string_(n),
      path: join(string_(n)),
      dir: true,
      playlist: false,
    })),
    // Playlists before tracks: they are what one is looking for when a folder
    // contains some, and a playlist drowned under a hundred files goes unseen.
    ...array(o.playlists).map((n) => ({
      name: string_(n),
      path: join(string_(n)),
      dir: false,
      playlist: true,
    })),
    ...array(o.files).map((n) => ({
      name: string_(n),
      path: join(string_(n)),
      dir: false,
      playlist: false,
    })),
  ]
  return {
    root: string_(o.root),
    path: base,
    query: string_(o.query),
    entries,
    // The search only reports audio files (see `scan::search`): none of them
    // is a playlist.
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
 * Recomposes the state of a wizard.
 *
 * Every absent field takes its empty value, never `undefined`: during a
 * deployment the plugin may be older than the page, and an `undefined` going
 * through a `v-for` would break the whole render instead of showing an empty
 * section.
 */
export function normalizeExploration(raw: unknown): Exploration {
  if (!raw) return EMPTY_EXPLORATION
  const o = raw as Record<string, unknown>
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
 * Truncates a path **from the start** so that it fits in `max` characters.
 *
 * From the start, and that is the whole point: on a path, the useful
 * information is the end — the folder we are in. No CSS property can do this:
 * `text-overflow` only cuts on the right, and `direction: rtl` would reorder
 * the segments instead of truncating them.
 *
 * **Whole segments** are removed as long as it is too long: cutting in the
 * middle of a name would give "…ents/My Music", where "…/My Music" keeps a
 * meaning. The final fallback only cuts inside a name if that name alone
 * exceeds the budget, for lack of anything better.
 */
export function truncateStart(path: string, max = 52): string {
  if (path.length <= max) return path
  const segments = path.split('/').filter(Boolean)
  let tail = ''
  for (let i = segments.length - 1; i >= 0; i -= 1) {
    const attempt = tail ? `${segments[i]}/${tail}` : segments[i]!
    // Two characters reserved for the "…/" that announces the cut.
    if (attempt.length + 2 > max) break
    tail = attempt
  }
  if (!tail) {
    const last = segments[segments.length - 1] ?? path
    return `…${last.slice(Math.max(0, last.length - max + 1))}`
  }
  return `…/${tail}`
}

/** Last segment of a relative path, `/` separator (the plugin's). */
export function leaf(path: string): string {
  const parts = path.split('/').filter(Boolean)
  return parts.length ? parts[parts.length - 1]! : path
}

export function normalizeData(raw: unknown): Data {
  const o = (raw ?? {}) as Record<string, unknown>
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
        // `=== true` / `=== false` and not a coercion: this is what tells
        // "present" apart from "unknown", the latter having to stay `null`
        // until display.
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
    // Entries of a loaded m3u that no rule could resolve: raw paths, the only
    // thing the user can match against their files.
    unresolved: array(o.unresolved).map(string_),
    browse: normalizeBrowse(o.browse),
    volumes: array(o.volumes).map((v) => {
      const e = (v ?? {}) as Record<string, unknown>
      return { path: string_(e.path), fstype: string_(e.fstype) }
    }),
    // False by default: better to grey out a usable wizard than to offer one
    // that will fail on click without saying why.
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
 * Readable duration. `0` (unknown duration, the case of a missing file or of a
 * container without header) renders as a dash rather than as "0:00", which
 * would assert an empty track.
 */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '—'
  const s = Math.round(seconds)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const r = s % 60
  const two = (n: number) => String(n).padStart(2, '0')
  return h ? `${h}:${two(m)}:${two(r)}` : `${m}:${two(r)}`
}

/** Label of a root's target: local path, or `//host/share/subpath`. */
export function rootTarget(r: Root): string {
  if (r.kind === 'local') return r.path
  const base = `//${r.host}/${r.share}`
  return r.subpath ? `${base}/${r.subpath}` : base
}
