import type { Mode } from '@ritornello/ui'

export interface PluginStatus {
  name: string
  kind: string
  connected: boolean
  admin: boolean
  /** Launched, not yet announced, and **past** the normal timeout: a diagnostic. */
  stalled?: boolean
  /** Launched just now, not yet announced, and that is normal. Mutually
   * exclusive with `stalled`, from which it only differs by the elapsed time —
   * but that difference is everything: "stalled" accuses, "starting" observes. */
  starting?: boolean
  disabled?: boolean
  /** Connected, but its admin page does not answer the ping: a long `set_data`
   * holds its lock (most often a network share). Computed at the time of
   * `/api/status`, so it can change from one refresh to the next. */
  busy?: boolean
  /** Fingerprint of the plugin's UI assets, when it announced one. Absent for
   * a plugin with no admin page, or one predating the field. */
  ui_version?: string
  /** Version of the plugin binary, as its announcement gave it. Absent for a
   * binary predating the field. */
  version?: string
  /** Where this plugin's releases live, as its announcement gave it: a full
   * URL, verbatim and unparsed. Absent for a plugin whose manifest names none
   * or one predating the field. Read by the core — a plugin announcing our own
   * repository is one of ours, anything else is third-party — and relayed here
   * for completeness; the update rows carry the interpreted `owner/repo` as
   * `ComponentOffer.third_party_repo`. */
  repository?: string
  /** Protocol this binary announced, present **only** when it differs from
   * this core's own (see `StatusPayload.protocol`). Its presence is the
   * refusal itself. */
  incompatible?: number
  /** Declared, and its binary is not on disk. Optional: absent when false. */
  missing_binary?: boolean
}
export interface StatusPayload {
  plugins: PluginStatus[]
  active_source: string
  /** Protocol this core speaks. The other half of what the configuration page
   * needs to explain a refused plugin: a line carries what its binary
   * claimed, this carries what the core expects. */
  protocol: number
  /**
   * Identifier of this run of the core, used by the shell as the `v=` stamp
   * of a plugin's catalog URL (see `PluginRoute.vue`'s `catalogQuery`).
   *
   * A restart of the core (which is also what a language pack edit ends
   * with) changes it, which is what lets the catalog be served `immutable`
   * without ever going stale.
   */
  session: string
  /**
   * The current UI language, always populated by the core (it defaults to
   * `en` server-side rather than omitting the field). Used as the `lang=`
   * stamp of a plugin's catalog URL, alongside `session`.
   */
  locale: string
}
/**
 * Where one component stands against the release, mirroring
 * `update::state::Availability` field for field.
 */
export type Availability =
  | 'aligned'
  | 'update_available'
  | 'binary_missing'
  | 'not_installed'
  | 'undeclared'
  | 'unknown'

/**
 * One row of `UpdatePayload.components`, mirroring `update::state::ComponentOffer`.
 *
 * `installed`, `offered`: `Option<T>` **without** `skip_serializing_if` on the
 * Rust side, so they serialize as `null` and are typed `T | null` here.
 * `installable`, `third_party_repo`, `not_installed_files` **do** carry that
 * attribute, so they are typed `T?` (absent, not `null`) — a different
 * statement: "unknown yet" versus "known to be nothing".
 */
export interface ComponentOffer {
  name: string
  kind: 'core' | 'plugin' | 'third_party'
  declared: boolean
  binary_present: boolean
  installed: string | null
  offered: string | null
  availability: Availability
  /** Absent until an archive has been read. `false` is the files plugin. */
  installable?: boolean
  /** For a `third_party` row: the repository its binary announced, as
   * `owner/repo` when that is a GitHub URL the core can address, and the raw
   * announced string when it is not — in which case the row is third-party and
   * **not checkable**, so `offered` stays `null` and `availability` stays
   * `unknown`. Absent on every other kind of row. */
  third_party_repo?: string
  /**
   * The core's own row only: what its last-installed archive carried outside
   * what the core ever places itself (the privileged installer, the systemd
   * units, the polkit rules). Absent until a core install has actually
   * happened — never an empty array, since the core's archive always carries
   * something here by design.
   */
  not_installed_files?: string[]
}

/** Left by the rollback unit, mirroring `ritornello_updater::rollback::Report`. */
export interface RollbackSummary {
  at_unix_s: number
  restored: string[]
  failed: string[]
  core_restored: boolean
}

/** The payload of `GET /api/update`, mirroring `update::state::UpdateState`. */
export interface UpdatePayload {
  /**
   * Tagged like the Rust enum: `{ kind, detail? }`. `installed` is the
   * transient report of a just-finished install (Ruling 55: added after this
   * type's first draft, adjacently tagged like `failed`) — it does not
   * replace the row flipping to "up to date", which is the durable signal.
   */
  outcome:
    | { kind: 'never_checked' }
    | { kind: 'no_release' }
    | { kind: 'ok' }
    | { kind: 'installed'; detail: string }
    | { kind: 'failed'; detail: string }
  release_version: string | null
  release_url: string | null
  last_check_unix_s: number | null
  components: ComponentOffer[]
  /** What is happening right now, as a catalog message, or `null` when idle. */
  busy: string | null
  /** Left by the rollback unit. `null` when nothing has ever rolled back. */
  last_rollback: RollbackSummary | null
}

export interface AudioDevice { name: string; description: string }
export interface AudioPayload { devices: AudioDevice[]; current: string | null }
export interface LocalePayload { locales: string[]; current: string | null }
export interface ThemePayload { theme: string; mode: Mode }
export interface LogsPayload { lines: string[] }
/** The three values of `settings.startup_power`, on the core side as on the UI side. */
export type StartupPower = 'on' | 'standby' | 'previous'

/**
 * The order of a date's components, as the device writes it. A closed choice
 * and not a free pattern: a faulty pattern would yield an empty display.
 */
export type DateFormat = 'day_month_year' | 'year_month_day' | 'month_day_year'

/** `off`, `check`, or `check_and_install`, mirroring `update::schedule::UpdatePolicy`. */
export type UpdatePolicy = 'off' | 'check' | 'check_and_install'
/** Mirrors `update::schedule::Weekday`. */
export type Weekday =
  | 'sunday' | 'monday' | 'tuesday' | 'wednesday' | 'thursday' | 'friday' | 'saturday'
/**
 * Mirrors `update::schedule::UpdateCadence`, adjacently tagged
 * (`#[serde(tag = "kind", content = "day")]`): daily carries no day, weekly
 * names one.
 */
export type UpdateCadence = { kind: 'daily' } | { kind: 'weekly'; day: Weekday }

/** Behavior settings, as served by `GET /api/settings`. */
/**
 * Where each piece of what is displayed comes from: the contributor retained
 * for each filled field, and those who searched without finding anything.
 */
export interface Provenance {
  /** By field name: `artist`, `title`, `album`, `year`, `duration`, `links`, `cover`. */
  fields?: Record<string, string>
  /** The plugins that searched and found nothing for this track. */
  misses?: string[]
  /**
   * Who **reworked** a field without being its source, by field name.
   *
   * Complements `fields` instead of replacing it: "Title: icy, split by
   * musicbrainz" states both facts.
   */
  derived?: Record<string, string>
}

/**
 * Snapshot of the cover cache, `GET /api/cover-cache`. Mirror of
 * `cover::CacheSnapshot` — the field names are the contract, and TypeScript
 * sees nothing of a JSON, so a rename on either side breaks the panel in
 * silence.
 */
export interface CachePayload {
  thumbnails: number
  thumbnails_bytes: number
  full_sizes: number
  full_sizes_bytes: number
  renditions_built: number
  /** Always `thumbnails_bytes + full_sizes_bytes` — see `CacheSnapshot`. */
  used_bytes: number
  budget_bytes: number
}

export interface SettingsPayload {
  volume_repeat_initial_ms: number
  volume_repeat_interval_ms: number
  /**
   * Behavior at service startup: `on` wakes the active source, `standby`
   * leaves the device in standby, `previous` resumes the state it had at the
   * last shutdown.
   */
  startup_power: StartupPower
  /**
   * The order of a date's components. Two separate settings and not a single
   * pattern: the order of a date and the 12/24 h choice do not vary together
   * from one country to another.
   */
  date_format: DateFormat
  /** 24 h clock (`13:05`) rather than 12 h (`1:05 PM`). */
  clock_24h: boolean
  /** Display duration of the volume/mute overlay and of the sources' ephemeral messages. */
  overlay_ms: number
  /** Input window of the remote's `+10` accumulation (time left for the second press). */
  tens_window_ms: number
  /**
   * Memory budget for the covers the core keeps at hand, in mebibytes.
   *
   * Outside the greyed-out re-encoding box: this bound applies no matter
   * what, like the source cap just below.
   */
  cover_cache_budget_mio: number
  /**
   * Cap on a cover **downloaded from the internet**, in mebibytes. The
   * counterpart of `cover_source_max_mio` for a third-party origin rather
   * than a trusted disk or share.
   */
  cover_download_max_mio: number
  /**
   * Cap of the **source** cover, in mebibytes. Always applied, whether
   * re-encoding is active or not — it is the only guard that remains when it
   * is unchecked, and the reason the UI takes it out of the greyed-out box.
   */
  cover_source_max_mio: number
  /** Re-encode covers into a thumbnail, or push the source as-is. */
  cover_rendition: boolean
  /** Longest side of the thumbnail, in pixels. Rendition only. */
  cover_max_edge_px: number
  /** JPEG quality of the thumbnail. Rendition only, and ignored if the image has an alpha channel. */
  cover_jpeg_quality: number
  /** Weight under which a cover is pushed without being re-encoded, in kibibytes. */
  cover_passthrough_max_ko: number
  /** Cap of pixels to decode, in megapixels. Rendition only. */
  cover_max_pixels_mpx: number
  /** Step of the "forward" / "rewind" keys, in seconds. */
  seek_step_s: number
  /** Nothing, check only, or check and install. See `UpdatePolicy`. */
  update_policy: UpdatePolicy
  /** Local hour (0-23) an automatic run may fire at. */
  update_hour: number
  /** How often an automatic run is due. */
  update_cadence: UpdateCadence
}
/**
 * State of the player, as pushed by `/api/player`: everything that is volatile.
 *
 * A single, flat object for a single panel. `/api/status` carries alongside
 * the navigation contract, structurally stable and read once at mount time —
 * which is why the volume is not in it.
 *
 * The track's fields are optional: any available information is displayed,
 * even partial. `origin` says who provided it — `"icy"` for what the stream
 * announces itself, otherwise the name of the `metadata` plugin that won.
 */
export interface PlayerPayload {
  source: string
  volume: number
  muted: boolean
  standby: boolean
  /**
   * Numbered key matching what is playing (radio preset, cd track), as the
   * active source declared it — this is what the remote highlights. `null`:
   * nothing is playing, or nothing declared.
   */
  preset: number | null
  /**
   * Number of presets the active source declares (radio stations, cd tracks),
   * or `null` if it does not declare it — the grid then falls back to the 9
   * historical bare keys. `0` is meaningful ("nothing to number", e.g. cd
   * without a disc) and distinct from `null`.
   */
  preset_count: number | null
  /**
   * Readable name the active source gives to the current preset (configured
   * station name for the radio), or `null` when it declares none (the cd, or
   * nothing playing). Lives and dies with `preset`.
   */
  preset_name: string | null
  /**
   * Already translated state sentence: the status declared by the source
   * ("NO DISC") or the standby word resolved by the core. `null` when there
   * is nothing to say.
   */
  status: string | null
  /**
   * Overlay in progress on the display side. The SPA ignores it — it already
   * shows the volume in plain text, and a browser screen does not have the
   * width constraints of a twenty-column display — but the field travels
   * because the payload is unique.
   */
  overlay: unknown | null
  artist: string | null
  title: string | null
  album: string | null
  /**
   * Release year, when a contributor knows it.
   *
   * Optional, like `links`: the core **omits** the field rather than emitting
   * a `null`, so that a frame without a year stays byte-for-byte identical to
   * what it was before this work.
   */
  year?: number | null
  /**
   * The listening platforms where this track can be found.
   *
   * Absent from the frame when the list is empty, hence optional: the core
   * omits the field rather than emitting an empty array. `platform` is a
   * closed set on the protocol side, and the URL has already been validated
   * against that platform's host — so the UI has nothing to re-check before
   * turning it into a link.
   */
  links?: { platform: 'youtube' | 'deezer' | 'apple_music'; url: string }[]
  duration_s: number | null
  origin: string | null
  /** Local URL of the cover, served by the device. Never an external URL. */
  cover_href: string | null
  /** Who provided the cover: name of the Source, `tags`, or name of the plugin. */
  cover_origin: string | null
  /**
   * Where each field comes from, and who searched without finding.
   *
   * Optional: the core omits it when it has nothing to say, and a frame
   * emitted by an earlier version does not carry it.
   */
  provenance?: Provenance
  /**
   * Where what is playing stands, in seconds, at the instant the frame was
   * published — the core pushes one per second during playback. `null` when
   * nobody knows: nothing is playing, or it is a stream that no `metadata`
   * plugin tracks.
   */
  position_s: number | null
  /**
   * What is playing accepts a seek. Distinct from "a duration is known":
   * Radio France announces the duration of a track on a live stream that
   * cannot be rewound. This field, and it alone, makes the bar clickable.
   */
  seekable: boolean
  /**
   * The active source has something to eject: this is what greys out the
   * Eject key anywhere but on the cd player. A capability of the **source**,
   * not of the content — an empty tray opens too.
   *
   * False by default, and absent from the frame when false (like `seekable`):
   * not knowing means offering nothing.
   */
  can_eject: boolean
  /**
   * The active source has a finite list to shuffle or repeat (radio has
   * none). This is the capability the web remote reads to grey out the
   * random/repeat-all keys — not `random`/`repeat_all` below, which say
   * whether a mode is *on*, a different question.
   *
   * Same convention as `can_eject`: `false` by default, not knowing means
   * offering nothing.
   */
  has_finite_list: boolean
  /**
   * Random play is on. The setting behind it is persisted like `volume` and
   * survives a source change or standby, but what arrives here is that
   * setting **masked by `has_finite_list`**: a mode the active source cannot
   * honour is a mode the device is not in, and the core refuses to arm one
   * there. So this is never `true` while `has_finite_list` is `false` —
   * which is what keeps the greyed keys on the radio from also showing
   * themselves as pressed.
   */
  random: boolean
  /** Repeat-all is on. Same masking and the same relation to `has_finite_list` as `random`. */
  repeat_all: boolean
  /**
   * What the player is doing: `playing`, `paused`, or absent when nothing is
   * playing. This is what picks the play button's icon (▶ or ❚❚). The field
   * was already travelling without being read.
   */
  playback?: Playback
}
/** `arg` also carries a boolean: `SetRandom`/`SetRepeatAll` send the absolute value of a mode. */
export type Command = { cmd: string; arg?: number | boolean }
/** What the player is doing. Absent from the frame when it is stopped (`seekable` idiom). */
export type Playback = 'playing' | 'paused'
/** A named preset as `GET /api/presets` serves it. */
export interface NamedPreset { index: number; name: string }
/** A source and its list; `presets` is absent when it does not enumerate. */
export interface SourcePresets { name: string; presets?: NamedPreset[] }
/** The catalog of sources, as the core broadcasts it to the displays. */
export interface PresetsPayload { sources: SourcePresets[] }
export interface SystemUsage { total_kb: number; available_kb: number }
/**
 * One process of the Ritornello tree, as served by
 * `GET /api/system/processes` behind the `(?)` of the Memory card.
 *
 * `percent` and `age_s` are nullable for the same reason as the fields of
 * `SystemPayload`: a machine whose `MemTotal` or uptime is unreadable still
 * has a list to show, it just cannot say what share of the whole a line is.
 */
export interface ProcessEntry {
  pid: number
  name: string
  rss_kb: number
  percent: number | null
  age_s: number | null
}

/** `GET /api/system/processes`. Never `null`: an empty list is an answer. */
export interface ProcessesPayload { processes: ProcessEntry[] }

/**
 * OS metrics, as served by `GET /api/system`.
 *
 * Any field the machine does not expose is `null` — no thermal sensor, no
 * cpufreq, no under-voltage probe — and the view displays "—" without
 * treating that as a failure. The key set, however, is stable.
 */
export interface SystemPayload {
  temperature_c: number | null
  cpu_mhz: number | null
  load: [number, number, number] | null
  cpus: number | null
  memory: SystemUsage | null
  disk: SystemUsage | null
  under_voltage: boolean | null
  /** Under-voltage that occurred since boot (sticky firmware bit), distinct
   *  from `under_voltage` (the instantaneous alarm): an episode lasts a few
   *  milliseconds to a few seconds and a 5 s probe has little chance of
   *  landing right on it, whereas this bit stays true until the next boot. */
  under_voltage_since_boot: boolean | null
  uptime_s: number | null
  service_uptime_s: number
  hostname: string | null
  ip: string | null
  os: string | null
  kernel: string | null
  version: string
  can_power_off: boolean
  can_reboot: boolean
  /**
   * Did logind answer the startup probe, whatever its answer was? Separates
   * the two causes of a greyed-out button: a refusal calls for the polkit
   * rule, no answer calls for a running `systemd-logind`. Two different
   * repairs, hence two sentences.
   */
  logind_reachable: boolean
  /**
   * Cumulative counters from `/proc/stat` since boot — never a percentage:
   * two tabs probing out of phase would corrupt a delta computed on the core
   * side. The view compares them between two successive probes.
   */
  cpu_total_jiffies: number | null
  cpu_idle_jiffies: number | null
}
