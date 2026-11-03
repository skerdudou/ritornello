// The shapes `get_data` serves, and what the page makes of the codes in them.

export type T = (key: string, params?: Record<string, string | number>) => string

export type Kind = 'video' | 'playlist'

/** A saved preset. */
export interface Entry {
  preset: number
  url: string
  kind: Kind
  title: string
  channel?: string | null
  count?: number | null
  /** The site, as yt-dlp names its extractor (`Soundcloud`, `BandcampAlbum`...). */
  site?: string | null
  /** What a video of another site than YouTube is known by: kept, never shown. */
  identity?: string | null
}

/** What a link can become: a preset to add, without its number yet. */
export interface Found {
  url: string
  kind: Kind
  title: string
  channel: string | null
  count: number | null
  site: string | null
  identity: string | null
}

/**
 * A failure carried as a **code** (translated here, on every render, against
 * the catalog the page holds) and, for what the tool or the system said, its
 * own words as `detail`.
 */
export interface Fault {
  code: string
  detail?: string
}

export type ProbeLine =
  | { line: string; state: 'pending' }
  | { line: string; state: 'found'; choices: Found[] }
  | { line: string; state: 'error'; code: string; detail?: string }

export interface Target {
  root: string
  path: string
}

export interface Explore {
  busy: boolean
  /** The last listing: the folder, and the names under it. */
  listing: { target: Target; names: string[] } | null
  error: Fault | null
}

export type RowState = 'pending' | 'running' | 'done' | 'failed' | 'skipped' | 'cancelled'

export interface JobRow {
  preset: number
  id: string
  /** Empty for a preset that no longer exists. */
  title: string
  state: RowState
  /** The reason of a failure or a skip: a code, and yt-dlp's own words as `detail`. */
  detail?: Fault
  percent?: number
}

export interface Job {
  id: number
  presets: number[]
  target: Target
  state: 'queued' | 'running' | 'done' | 'cancelled' | 'failed'
  detail?: Fault
  rows: JobRow[]
}

export interface UpdateFault {
  code: string
  params?: Record<string, string>
}

export interface ToolStatus {
  installed: boolean
  version: string | null
  channel: 'stable' | 'nightly'
  updating: boolean
  waiting_for_jobs: boolean
  /** Why the last update failed: a code, and parameters (versions, the raw text of the network or the system). */
  error_code: UpdateFault | null
  /** The failure is a refusal that "install anyway" takes. */
  forceable: boolean
  last_check: number | null
  /** Why the plugin's own JavaScript runtime could not be installed. */
  runtime_error_code: UpdateFault | null
}

export interface Capabilities {
  python3: string | null
  js_runtime: string | null
  js_runtime_slow: boolean
  /** The exact command that would fix what is missing. */
  apt_hint: string | null
  checking: boolean
  /** No runtime exists for this device today: none could help. */
  js_hopeless: boolean
}

/** The last playback that failed, as the wrapper recorded it for mpv's runs. */
export interface PlaybackFailure {
  /** Seconds since the epoch. */
  time: number
  /** The host of the address that failed; empty when it had none. */
  host: string
  code: string
  /** yt-dlp's last line, its own words. */
  detail: string
}

export interface Data {
  entries: Entry[]
  probe: { running: boolean; results: ProbeLine[] }
  /** The shares a download may go to. */
  targets: string[]
  last_target: Target | null
  explore: Explore
  jobs: Job[]
  tool: ToolStatus
  capabilities: Capabilities
  last_failure: PlaybackFailure | null
}

/** What an inert page shows when the first read failed: nothing, and nothing running. */
export function emptyData(): Data {
  return {
    entries: [],
    probe: { running: false, results: [] },
    targets: [],
    last_target: null,
    explore: { busy: false, listing: null, error: null },
    jobs: [],
    tool: {
      installed: false, version: null, channel: 'stable', updating: false, waiting_for_jobs: false,
      error_code: null, forceable: false, last_check: null, runtime_error_code: null,
    },
    capabilities: {
      python3: null, js_runtime: null, js_runtime_slow: false, apt_hint: null, checking: false, js_hopeless: false,
    },
    last_failure: null,
  }
}

/**
 * Is there work whose end the page is waiting for? (The protocol pushes nothing.)
 *
 * A first install counts from before it starts: with python3 there, nothing
 * installed and no failure said, the plugin is about to install (at its
 * start, or after "Check again"), and a page opened in that moment must see
 * it end. A failed install says so and stops the reading.
 */
export function workInProgress(d: Data | null): boolean {
  if (!d) return false
  return (
    (!d.tool.installed && d.capabilities.python3 !== null && d.tool.error_code === null) ||
    d.probe.running ||
    d.explore.busy ||
    d.tool.updating ||
    d.tool.waiting_for_jobs ||
    d.capabilities.checking ||
    d.jobs.some((j) => j.state === 'queued' || j.state === 'running')
  )
}

/**
 * The sentence for a code. Literal keys, one per code: the guard that checks
 * every key the page uses against the catalog (`i18nKeysUsed.test.ts`) only
 * sees keys written out.
 */
export function faultText(t: T, fault: Fault): string {
  const detail = fault.detail ?? ''
  switch (fault.code) {
    case 'not_a_url':
      return t('fault_not_a_url')
    case 'foreign_host':
      return t('fault_foreign_host', { detail })
    case 'no_media':
      return t('fault_no_media')
    case 'live_stream':
      return t('fault_live_stream')
    case 'local_network':
      return t('fault_local_network', { detail })
    case 'tool_missing':
      return t('fault_tool_missing')
    case 'timeout':
      return t('fault_timeout')
    case 'unknown_root':
      return t('fault_unknown_root')
    case 'not_writable':
      return t('fault_not_writable')
    case 'not_mounted':
      return t('fault_not_mounted')
    case 'escapes':
      return t('fault_escapes')
    case 'unresponsive':
      return t('fault_unresponsive')
    case 'io':
      return t('fault_io', { detail })
    case 'wrong_kind':
      return t('fault_wrong_kind')
    case 'unavailable':
      return t('fault_unavailable')
    case 'invalid_id':
      return t('fault_invalid_id')
    case 'timed_out':
      return t('fault_timed_out')
    case 'interrupted':
      return t('fault_interrupted')
    case 'job_lost':
      return t('fault_job_lost')
    case 'no_such_preset':
      return t('fault_no_such_preset')
    case 'preset_no_playlist':
      return t('fault_preset_no_playlist')
    case 'preset_no_video':
      return t('fault_preset_no_video')
    case 'bad_entry':
      return t('fault_bad_entry')
    case 'failed_silent':
      return t('fault_failed_silent')
    case 'exited':
      return t('fault_exited', { detail })
    case 'unreadable_output':
      return t('fault_unreadable_output', { detail })
    default:
      // `failed`, and a code a newer plugin may add: the tool's own words.
      return t('fault_failed', { detail: detail || fault.code })
  }
}

/** Why the last update failed, in the page's language. One key per code. */
export function updateErrorText(t: T, fault: UpdateFault): string {
  const p = fault.params ?? {}
  const detail = p.detail ?? ''
  const offered = p.offered ?? ''
  const installed = p.installed ?? ''
  switch (fault.code) {
    case 'busy':
      return t('update_busy')
    case 'python3_missing':
      return t('update_python3_missing')
    case 'python3_unusable':
      return t('update_python3_unusable', { detail })
    case 'fetch':
      return t('update_fetch', { detail })
    case 'refused_bad_signature':
      return t('update_refused_bad_signature', { detail })
    case 'refused_not_listed':
      return t('update_refused_not_listed')
    case 'refused_mismatch':
      return t('update_refused_mismatch')
    case 'rollback_older':
      return t('update_rollback_older', { offered, installed })
    case 'rollback_other_build':
      return t('update_rollback_other_build', { offered })
    case 'rollback_unreadable':
      return t('update_rollback_unreadable', { installed })
    case 'rollback_other':
      return t('update_rollback_other', { detail })
    case 'install':
      return t('update_install', { detail })
    case 'install_no_package':
      return t('update_install_no_package')
    case 'install_does_not_start':
      return t('update_install_does_not_start', { detail })
    case 'install_no_version':
      return t('update_install_no_version', { detail })
    case 'install_swap':
      return t('update_install_swap', { detail })
    case 'install_not_zip':
      return t('update_install_not_zip', { detail })
    case 'install_extract_limit':
      return t('update_install_extract_limit')
    case 'install_extract_unsafe':
      return t('update_install_extract_unsafe')
    case 'install_write':
      return t('update_install_write', { detail })
    default:
      // A code a newer plugin may add: show it rather than nothing.
      return fault.code
  }
}

/** Why the plugin's own JavaScript runtime (deno) could not be installed. */
export function runtimeErrorText(t: T, fault: UpdateFault): string {
  const p = fault.params ?? {}
  const detail = p.detail ?? ''
  switch (fault.code) {
    case 'no_build':
      return t('runtime_no_build', { arch: p.arch ?? '' })
    case 'fetch':
      return t('runtime_fetch', { detail })
    case 'not_listed':
      return t('runtime_not_listed')
    case 'mismatch':
      return t('runtime_mismatch')
    case 'extract':
      return t('runtime_extract', { detail })
    case 'write':
      return t('runtime_write', { detail })
    default:
      return fault.code
  }
}

/** Why the last playback failed, in the page's language. One key per code. */
export function playFailureText(t: T, code: string): string {
  switch (code) {
    case 'guard_refused':
      return t('play_failed_guard_refused')
    case 'live_refused':
      return t('play_failed_live_refused')
    case 'no_formats':
      return t('play_failed_no_formats')
    default:
      // `ytdlp_failed`, and a code a newer plugin may add.
      return t('play_failed_ytdlp_failed')
  }
}

/** A time the plugin sent (seconds since the epoch), as the browser's locale writes it. */
export function whenText(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString()
}

/**
 * What a download row is called: its title; for a video that has none
 * (a set's track with no usable name), "track n", its place in its preset,
 * as the device's status line says it; for a preset that no longer exists,
 * its number.
 */
export function rowTitle(t: T, rows: JobRow[], i: number): string {
  const r = rows[i]!
  if (r.title) return r.title
  if (!r.id) return t('row_preset', { preset: r.preset })
  const first = rows.findIndex((x) => x.preset === r.preset)
  return t('untitled_track', { n: i - first + 1 })
}

/** "1 video", "12 videos". */
export function videoCount(t: T, count: number): string {
  return count === 1 ? t('count_videos_one') : t('count_videos', { count })
}

export function stateText(t: T, state: string): string {
  switch (state) {
    case 'queued':
      return t('state_queued')
    case 'running':
      return t('state_running')
    case 'done':
      return t('state_done')
    case 'cancelled':
      return t('state_cancelled')
    case 'failed':
      return t('state_failed')
    case 'pending':
      return t('state_pending')
    case 'skipped':
      return t('state_skipped')
    default:
      return state
  }
}

export function targetText(target: Target): string {
  return target.path ? `${target.root}/${target.path}` : target.root
}
