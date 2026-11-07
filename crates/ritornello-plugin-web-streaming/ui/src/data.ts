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

export type RowState = 'pending' | 'running' | 'converting' | 'done' | 'failed' | 'skipped' | 'cancelled'

/** The format a download is kept in. */
export type Format = 'original' | 'm4a' | 'mp3'

/** Where a single video's cover goes. */
export type CoverMode = 'embedded' | 'separate'

/** What the operator chose in the download dialog. */
export interface Choices {
  format: Format
  cover: CoverMode
  /** The name of each preset's file (a video) or folder (a playlist), by its number. */
  names: Record<string, string>
  /** The part kept of a single video, by its number; absent, the whole. */
  trims: Record<string, Trim>
}

/** The part of a video kept, in whole seconds; a bound left out is its start or its end. */
export interface Trim {
  start?: number
  end?: number
}

/**
 * A time as typed: seconds (`95`), `m:ss` (`1:35`) or `h:mm:ss`
 * (`1:01:35`). Empty is `null`, no bound; anything else that does not read
 * as a time is `undefined`. Past the first part, minutes and seconds stay
 * under 60: `1:75` is a typo, not 2:15.
 */
export function parseClock(text: string): number | null | undefined {
  const typed = text.trim()
  if (!typed) return null
  const parts = typed.split(':')
  if (parts.length > 3 || parts.some((p) => !/^\d+$/.test(p))) return undefined
  const [first, ...rest] = parts.map(Number)
  if (rest.some((n) => n >= 60)) return undefined
  return rest.reduce((total, n) => total * 60 + n, first!)
}

export interface JobRow {
  preset: number
  id: string
  /** Empty for a preset that no longer exists. */
  title: string
  state: RowState
  /** The reason of a failure or a skip: a code, and yt-dlp's own words as `detail`. */
  detail?: Fault
  percent?: number
  /** What went otherwise than asked: a code, and ffmpeg's own words as `detail`. */
  notes?: Fault[]
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
  /** The runtime is the plugin's own node 22, past its end of life: kept as it is. */
  js_runtime_unmaintained: boolean
  /** ffmpeg's version; null without it (no mp3, no m4a conversion). */
  ffmpeg: string | null
  /** The command that installs ffmpeg, when it is missing. */
  ffmpeg_hint: string | null
}

/** How each runtime is usually written; the wire carries yt-dlp's names. */
export const RUNTIME_NAMES: Record<string, string> = { deno: 'Deno', node: 'Node.js', quickjs: 'QuickJS' }

export function runtimeName(name: string): string {
  return RUNTIME_NAMES[name] ?? name
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
  /** The last format and cover chosen. */
  last_format: Format
  last_cover: CoverMode
  /** The name a download would give each preset, by its number: the dialog's default. */
  default_names: Record<string, string>
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
    last_format: 'original',
    last_cover: 'separate',
    default_names: {},
    explore: { busy: false, listing: null, error: null },
    jobs: [],
    tool: {
      installed: false, version: null, channel: 'stable', updating: false, waiting_for_jobs: false,
      error_code: null, forceable: false, last_check: null, runtime_error_code: null,
    },
    capabilities: {
      python3: null, js_runtime: null, js_runtime_slow: false, apt_hint: null, checking: false, js_hopeless: false,
      js_runtime_unmaintained: false, ffmpeg: null, ffmpeg_hint: null,
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
    case 'no_ffmpeg':
      return t('fault_no_ffmpeg')
    case 'trim_playlist':
      return t('fault_trim_playlist')
    case 'trim_outside':
      return t('fault_trim_outside')
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

/** What a download row says besides its state. One key per code. */
export function noteText(t: T, note: Fault): string {
  switch (note.code) {
    case 'already_present':
      return t('note_already_present')
    case 'no_ffmpeg':
      return t('note_no_ffmpeg')
    case 'converted_name_taken':
      return t('note_converted_name_taken')
    case 'conversion_failed':
      return t('note_conversion_failed', { detail: note.detail ?? '' })
    case 'trim_failed':
      return t('note_trim_failed', { detail: note.detail ?? '' })
    case 'cover_beside':
      return t('note_cover_beside')
    case 'cover_name_taken':
      return t('note_cover_name_taken')
    default:
      // A code a newer plugin may add: show it rather than nothing.
      return note.code
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

/**
 * Why the plugin's own JavaScript runtime (deno, or node on armv7) could not
 * be installed or updated. `runtime` is its wire name, shown as usually
 * written.
 */
export function runtimeErrorText(t: T, fault: UpdateFault): string {
  const p = fault.params ?? {}
  const detail = p.detail ?? ''
  const runtime = runtimeName(p.runtime ?? '')
  switch (fault.code) {
    case 'no_build':
      return t('runtime_no_build', { runtime, arch: p.arch ?? '' })
    case 'fetch':
      return t('runtime_fetch', { detail })
    case 'bad_signature':
      return t('runtime_bad_signature', { runtime, detail })
    case 'not_listed':
      return t('runtime_not_listed')
    case 'unexpected':
      return t('runtime_unexpected', { detail })
    case 'mismatch':
      return t('runtime_mismatch')
    case 'extract':
      return t('runtime_extract', { detail })
    case 'version':
      return t('runtime_version', { runtime, detail })
    case 'write':
      return t('runtime_write', { runtime, detail })
    case 'rollback':
      return t('runtime_rollback', { runtime, installed: p.installed ?? '', listed: p.listed ?? '' })
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
    case 'converting':
      return t('state_converting')
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
