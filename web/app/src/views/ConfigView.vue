<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, Input,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import CoverCacheDetails from '../components/CoverCacheDetails.vue'
import InstallablesDialog from '../components/InstallablesDialog.vue'
import LanguageCard from '../components/LanguageCard.vue'
import LanguagePacksRow from '../components/LanguagePacksRow.vue'
import UpdateCard from '../components/UpdateCard.vue'
import UpdateDialog from '../components/UpdateDialog.vue'
import { predictedThumbnailBytes } from '../composables/coverWeight'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import { usePlugins } from '../composables/usePlugins'
import type {
  AudioPayload, LanguageBusy, LanguagePackRow, LocalePayload, SettingsPayload, UpdatePayload,
} from '../types'

const { t, reload } = useCatalog()
// The plugin state comes from the module, not from a local `ref`: the top
// navigation reads the **same** object, so a toggle made here updates its menu
// without a reload. See `usePlugins`.
const { state: status, refresh: refreshPlugins } = usePlugins()
const audio = ref<AudioPayload>({ devices: [], current: null })
// `completeness`/`fallback_candidates` default to a single, always-true "en"
// entry rather than empty arrays: `LanguageCard`'s `showFallback` looks up
// `current` in `completeness` before the first `GET /api/locale` answers,
// and an empty array there would just as correctly read "unknown, hide the
// control" — but `fallback_candidates` must never be empty even transiently
// (a `Select` with an empty item list renders nothing to open), so both are
// seeded consistently rather than asymmetrically.
const locale = ref<LocalePayload>({
  locales: [],
  current: null,
  completeness: [],
  fallback_current: 'en',
  fallback_candidates: ['en'],
  packs: [],
})
const device = ref('')
const lang = ref('')
// The language `loadAll()` last read from the server, kept apart from `lang`
// (the Select's own binding): `saveDisplay` compares the two to decide
// whether the locale route needs a write at all.
const loadedLocale = ref('')
// Same convention as `lang`/`loadedLocale`, for the fallback: `fallback` is
// `LanguageCard`'s v-model, `loadedFallback` is what the server last held.
const fallback = ref('en')
const loadedFallback = ref('en')
const audioUnavailable = ref(false)
const settings = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  startup_power: 'on',
  date_format: 'day_month_year',
  clock_24h: true,
  overlay_ms: 5000,
  tens_window_ms: 5000,
  seek_step_s: 10,
  cover_cache_budget_mio: 50,
  cover_download_max_mio: 2,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_passthrough_max_ko: 150,
  cover_max_pixels_mpx: 16,
  update_policy: 'off',
  update_hour: 3,
  update_cadence: { kind: 'daily' },
  update_prereleases: false,
})

/**
 * The device's own view of itself against the last release it read.
 * `never_checked` with no components is exactly `UpdateState::initial`'s own
 * shape before the first `GET /api/update` answers — the same convention as
 * `audio`/`locale` just above.
 */
const update = ref<UpdatePayload>({
  outcome: { kind: 'never_checked' },
  release_version: null,
  release_url: null,
  last_check_unix_s: null,
  components: [],
  busy: null,
  last_rollback: null,
})

/**
 * The core's own internal cap on the number of cache entries
 * (`cover.rs::MAX_ENTRIES`). It is **not** a memory bound — the byte budget
 * alone governs eviction — and must never be *labelled* as one: it exists
 * only so a pathological setting combination (e.g. re-encoding off, every
 * cover local) cannot make the estimate below print an unbounded number.
 *
 * **The figure does reach the page**, and a comment here used to deny it. Both
 * estimates take `min` with it so neither overstates what the cache would
 * actually hold, and that `min` bites at ordinary settings: budget 256 MiB
 * with a 16 KiB pass-through threshold clamps the typical count to 256, and
 * re-encoding off with a 1 MiB download cap clamps the floor to it too. What
 * must not happen is the page *explaining* the number — hence the wording of
 * `cover_cache_estimate_unlimited`, which states a few-hundred ceiling in
 * prose rather than naming a constant the user cannot interpret.
 */
const MAX_CACHE_ENTRIES = 256

/** Budget for the cache, in bytes, as entered by the user. */
const coverBudgetBytes = computed(
  () => (Number(settings.value.cover_cache_budget_mio) || 0) * 1024 * 1024,
)

/** Cap on a cover downloaded from the internet, in bytes. */
const coverDownloadBytes = computed(
  () => (Number(settings.value.cover_download_max_mio) || 0) * 1024 * 1024,
)

/**
 * What one thumbnail is predicted to weigh, in bytes — zero when re-encoding
 * is off (none is produced at all) or while a box is momentarily empty.
 */
const coverPredictedBytes = computed(() =>
  settings.value.cover_rendition
    ? predictedThumbnailBytes(
        Number(settings.value.cover_max_edge_px),
        Number(settings.value.cover_jpeg_quality),
      )
    : 0,
)

/**
 * What one **entry** of a local library costs the budget, in bytes.
 *
 * The conservative form, and the distinction matters: a cover light enough to
 * pass untouched is charged its own weight, which can reach the threshold, so
 * the threshold is the honest per-entry figure whenever it is the larger of
 * the two. Dividing the budget by the predicted weight while the page
 * announces a threshold twice as large would overstate the count.
 */
const coverEntryBytes = computed(() => {
  if (!settings.value.cover_rendition) return 0
  const threshold = (Number(settings.value.cover_passthrough_max_ko) || 0) * 1024
  return Math.max(coverPredictedBytes.value, threshold)
})

/**
 * Floor of the number of covers the budget holds at once: the worst case
 * where every entry is a network cover paying both its downloaded bytes and
 * its thumbnail. Always finite — the download cap cannot be zero — so this
 * one never needs the "unlimited" escape hatch below.
 *
 * **Never below one**, and that is not cosmetic rounding. The combination
 * exists: an 8 MiB budget with a 20 MiB download cap and a 2048 KiB
 * pass-through threshold (`Math.floor(8 / 22)`) floors to zero, and the page
 * then read "at least 0 covers" — which is both alarming and false.
 * `cover.rs::evict_to_budget` protects the entry its caller just inserted
 * (`keep_entry`), so a budget too small for even one cover still serves that
 * one rather than discarding it on arrival. One is therefore what the core
 * actually guarantees.
 */
const coverFloorEstimate = computed(() => {
  const perEntry = coverDownloadBytes.value + coverEntryBytes.value
  if (perEntry <= 0) return MAX_CACHE_ENTRIES
  return Math.min(MAX_CACHE_ENTRIES, Math.max(1, Math.floor(coverBudgetBytes.value / perEntry)))
})

/**
 * Typical count for a library of local covers, which pay only their
 * entry cost. `null` selects the sentence for "re-encoding is off": a local
 * entry then costs nothing at all (only a path — see `payload_cost` in
 * cover.rs), so there is no per-entry figure to divide the budget by.
 *
 * **Gated on the switch, not on the byte value**, and that distinction is a
 * fix. `coverEntryBytes` is also zero while the edge and threshold boxes are
 * both momentarily empty — clearing a number input to retype it is an
 * ordinary keystroke — and testing the bytes made the page announce
 * "re-encoding is off" with the switch visibly on. The switch is the only
 * thing that answers the question the sentence asks.
 *
 * A blank box with the switch on falls to the same `MAX_CACHE_ENTRIES` clamp
 * the floor already uses: a transient figure for a transient state, and the
 * only alternative — dividing by zero — prints `Infinity`.
 */
const coverTypicalEstimate = computed<number | null>(() => {
  if (!settings.value.cover_rendition) return null
  if (coverEntryBytes.value <= 0) return MAX_CACHE_ENTRIES
  return Math.min(MAX_CACHE_ENTRIES, Math.floor(coverBudgetBytes.value / coverEntryBytes.value))
})

/** The predicted weight, or `null` while there is no figure worth showing. */
const coverPredictedText = computed(() =>
  coverPredictedBytes.value > 0
    ? t.value('cover_predicted_weight', { kio: Math.round(coverPredictedBytes.value / 1024) })
    : null,
)

/**
 * The live estimate shown at the foot of the card: it now depends on nearly
 * every setting above it, so its sentence names all three inputs — the
 * budget, the download ceiling and the cost of one entry — rather than
 * leaving the reader to guess what moves it.
 */
const coverCacheEstimateText = computed(() =>
  coverTypicalEstimate.value === null
    ? t.value('cover_cache_estimate_unlimited', { floor: coverFloorEstimate.value })
    : t.value('cover_cache_estimate', {
        budget: Number(settings.value.cover_cache_budget_mio) || 0,
        download: Number(settings.value.cover_download_max_mio) || 0,
        entry: Math.round(coverEntryBytes.value / 1024),
        floor: coverFloorEstimate.value,
        typical: coverTypicalEstimate.value,
      }),
)

/**
 * View value for "Default (system)": never sent as is ("Change" translates it
 * into `device: null`), and impossible to confuse with an ALSA PCM name.
 */
const SYSTEM_DEFAULT = '__system_default__'

// The current selection may name a device that disappeared (unplugged card):
// we keep it visible at the end of the list rather than leaving the trigger
// empty.
const devices = computed(() => {
  const list = [...audio.value.devices]
  const current = audio.value.current
  if (current && !list.some((d) => d.name === current)) {
    list.push({ name: current, description: '' })
  }
  return list
})

/**
 * Labels of the trigger of each `Select` on this page, computed here rather
 * than left to `SelectValue`'s own text.
 *
 * reka-ui hands an option's text to its Select when that item **mounts**
 * (`SelectItemText`, `onMounted` → `onOptionAdd`) and never re-reads it. So a
 * label coming from `t()` freezes at the language it had then: changing the
 * language from the picker just below reloads the catalog, every other label
 * on the page follows — and these triggers kept the old language until the
 * list was opened, opening being what remounts the items and heals it.
 *
 * Reported from use, twice, on two different pages. A plain reactive binding
 * cannot go stale that way, and `SelectValue` renders the slot it is given in
 * preference to its captured text — which is what `SystemView` already did.
 *
 * The audio one is not a catalog label alone: the system default is
 * translated, the devices are named by the server. It still needs the same
 * treatment for its first entry, and gets the whole thing so there is one rule
 * on this page rather than two.
 */
const deviceLabel = computed(() => {
  if (device.value === SYSTEM_DEFAULT) return t.value('audio_default_device')
  const found = devices.value.find((d) => d.name === device.value)
  return found?.description || found?.name || device.value
})

const startupLabel = computed(() => {
  switch (settings.value.startup_power) {
    case 'on':
      return t.value('startup_on')
    case 'standby':
      return t.value('startup_standby')
    default:
      return t.value('startup_previous')
  }
})

const dateFormatLabel = computed(() => {
  switch (settings.value.date_format) {
    case 'year_month_day':
      return t.value('clock_date_ymd')
    case 'month_day_year':
      return t.value('clock_date_mdy')
    default:
      return t.value('clock_date_dmy')
  }
})

const clockHoursLabel = computed(() =>
  settings.value.clock_24h ? t.value('clock_24h') : t.value('clock_12h'),
)

async function loadAll() {
  // Needed here, not redundant: this is what reloads the catalog after a
  // successful language change (see `saveDisplay` below), in place of the
  // old `location.reload()`.
  await reload()
  // Re-reads the plugin state **and** arms the watch over the "stalled" window
  // that a re-enable has just opened: the core replaces the line as soon as the
  // plugin announces itself, a few seconds later, and without this re-read the
  // line stayed on "stalled" until the next F5.
  await refreshPlugins()
  audioUnavailable.value = false
  audio.value = await api.get<AudioPayload>('/api/audio-output').catch(() => {
    audioUnavailable.value = true
    return audio.value
  })
  locale.value = await api.get<LocalePayload>('/api/locale').catch(() => locale.value)
  settings.value = await api.get<SettingsPayload>('/api/settings').catch(() => settings.value)
  update.value = await api.get<UpdatePayload>('/api/update').catch(() => update.value)
  // `current: null` = no saved choice: the "Default (system)" entry carries it
  // — no more fallback to the first device (it was `null`, the PCM that
  // discards the sound, at the top of `aplay -L`).
  device.value = audio.value.current ?? SYSTEM_DEFAULT
  lang.value = locale.value.current ?? 'en'
  loadedLocale.value = lang.value
  fallback.value = locale.value.fallback_current
  loadedFallback.value = fallback.value
}

onMounted(loadAll)

interface PluginRow {
  name: string
  kinds: string
  connected: boolean
  stalled: boolean
  starting: boolean
  disabled: boolean
  busy: boolean
  admin: boolean
  version?: string
  incompatible?: number
  /** This plugin's announcement carried no `catalog` field at all — a binary
   * built before this core could ask for its embedded translation layers.
   * Distinct from an announced, empty catalog, which has no text of its own
   * and is not this. */
  catalog_unknown: boolean
  /** Declared in plugins.toml, and its binary is not on disk. */
  missing_binary: boolean
  /** A binary on disk that nothing declares — the twin of `missing_binary`. */
  undeclared_binary: boolean
  /**
   * Is this row's move index meaningful? Only a name `plugins.toml` actually
   * declares can be reordered — an `undeclared_binary` row has no line in
   * that file for `move_entry` to act on.
   */
  declared: boolean
  /**
   * The version `/api/update` currently offers this component, from the same
   * row `ComponentOffer` carries it on — `null` when no offer exists (never
   * checked, dropped out of the release window, or a release that never
   * carried this name). Ruling 88's guard, applied here to the table's own
   * Install button for the same reason it applies to the dialog's switch: a
   * row with nothing to install cannot be selected for installing, and
   * without this a `missing_binary` row whose release does not carry it
   * would press "Install" into `Resolved::Nothing` — refused, but silently
   * enough from the operator's chair that the guard is worth having anyway.
   */
  offered: string | null
  /** The bare file name to erase for "Remove the binary" — present only when
   * `undeclared_binary` is true. Never the same string as `name` once the
   * release's own convention applies to it (`ritornello-plugin-<name>`). */
  binary_file?: string
  /**
   * This row's binary is already being erased: an uninstall queued it, or
   * "Remove the binary" was pressed. It replaces both of the gestures an
   * `undeclared_binary` row licenses, because both would ask for something
   * already in flight — which is what made an ordinary uninstall read as a
   * job left half done.
   */
  removal_pending: boolean
}

/** Intermediate accumulator: the raw kinds, before we decide what must stay in
 * `kinds`. An array rather than a string built along the way, so that this
 * choice does not depend on arrival order. */
interface PluginAccumulator {
  name: string
  receivedKinds: string[]
  connected: boolean
  stalled: boolean
  starting: boolean
  disabled: boolean
  busy: boolean
  admin: boolean
  version?: string
  incompatible?: number
  catalog_unknown: boolean
  missing_binary: boolean
  undeclared_binary: boolean
  binary_file?: string
  removal_pending: boolean
}

/**
 * One row per plugin, its kinds joined. The table used to show one
 * (name, kind) pair per row; the toggle applies to the name, and three switches
 * that all do the same thing mean nothing.
 *
 * A plugin is "connected" only if **all** its kinds are: an unreachable half is
 * a problem, and the aggregate must not hide it.
 */
const plugins = computed<PluginRow[]>(() => {
  const byName = new Map<string, PluginAccumulator>()
  for (const p of status.value.plugins) {
    const acc = byName.get(p.name)
    if (!acc) {
      byName.set(p.name, {
        name: p.name,
        receivedKinds: [p.kind],
        connected: p.connected,
        stalled: !!p.stalled,
        starting: !!p.starting,
        disabled: !!p.disabled,
        busy: !!p.busy,
        admin: p.admin,
        version: p.version,
        incompatible: p.incompatible,
        catalog_unknown: !!p.catalog_unknown,
        missing_binary: !!p.missing_binary,
        undeclared_binary: !!p.undeclared_binary,
        binary_file: p.binary_file,
        removal_pending: !!p.removal_pending,
      })
      continue
    }
    acc.receivedKinds.push(p.kind)
    acc.connected = acc.connected && p.connected
    acc.stalled = acc.stalled || !!p.stalled
    acc.starting = acc.starting || !!p.starting
    acc.disabled = acc.disabled || !!p.disabled
    acc.busy = acc.busy || !!p.busy
    acc.admin = acc.admin || p.admin
    // All lines of a plugin carry the same version and the same refusal: the
    // first one to define it suffices. `??`, not `||`, so a refusal at
    // protocol 0 (were that ever to happen) is not mistaken for "none".
    acc.version = acc.version ?? p.version
    acc.incompatible = acc.incompatible ?? p.incompatible
    // OR, like `stalled`/`busy`: every kind of a given plugin carries the
    // same fact, derived from the same single announcement, so this is not
    // really a choice between kinds — it only guards against a kind that
    // renders no line at all leaving the flag stuck at its initial `false`.
    acc.catalog_unknown = acc.catalog_unknown || !!p.catalog_unknown
    acc.missing_binary = acc.missing_binary || !!p.missing_binary
    acc.undeclared_binary = acc.undeclared_binary || !!p.undeclared_binary
    acc.binary_file = acc.binary_file ?? p.binary_file
    acc.removal_pending = acc.removal_pending || !!p.removal_pending
  }
  const declaredRows: PluginRow[] = [...byName.values()].map((acc) => {
    // "unknown" is never shown next to a real kind: we only keep it when it is
    // the only information received for this name. This holds by construction,
    // over the complete set of received kinds — not by looking only at what the
    // accumulator held at a given instant, which would depend on the arrival
    // order of the lines.
    //
    // A row whose every received kind is "unknown" reads "—", not the word
    // "unknown": that word never announced anything real. Both the ordinary
    // "not yet announced" rows and an `undeclared_binary` line get the same
    // dash (review of task 18, M6).
    const realKinds = acc.receivedKinds.filter((k) => k !== 'unknown')
    // Through the same `plugin_kind_*` catalog keys `InstallablesDialog.vue`
    // uses for the same four words (m5): this column used to show the raw
    // wire string (`source`, `display`…) while the dialog already translated
    // it, the one surface in a French interface still speaking English.
    // A template literal, not concatenation, for the same reason as there:
    // `i18nKeysUsed.test.ts`'s literal-key scanner only recognises a quoted
    // string immediately after `t(`, and the four keys are already on its
    // explicit list.
    //
    // A kind outside the closed vocabulary still renders its raw
    // `plugin_kind_<word>` catalog key here, exactly as it would in the
    // dialog (F4): filtering it would need the same closed list hard-coded a
    // fourth time (`plugin_catalogue_declaration.rs`, the scanner's
    // allow-list, and the four locale keys already are three), which is F6's
    // question to answer once, not this fix's to answer again here.
    const kinds = realKinds.length > 0 ? realKinds.map((k) => t.value(`plugin_kind_${k}`)).join(', ') : '—'
    // Looked up by name rather than carried through the accumulator: the
    // offer lives on a wholly different payload (`/api/update`), read once
    // here rather than threaded through every accumulator field above.
    const offer = update.value.components.find((c) => c.name === acc.name)
    return {
      name: acc.name,
      kinds,
      connected: acc.connected,
      stalled: acc.stalled,
      starting: acc.starting,
      disabled: acc.disabled,
      busy: acc.busy,
      admin: acc.admin,
      version: acc.version,
      incompatible: acc.incompatible,
      catalog_unknown: acc.catalog_unknown,
      missing_binary: acc.missing_binary,
      undeclared_binary: acc.undeclared_binary,
      // Only a name `plugins.toml` truly declares can be reordered.
      // `undeclared_binary` is the one flag among these rows that means
      // "not declared" — everything else here (including `missing_binary`)
      // has a `[[plugin]]` block, `move_entry`'s own unit.
      declared: !acc.undeclared_binary,
      offered: offer?.offered ?? null,
      binary_file: acc.binary_file,
      removal_pending: acc.removal_pending,
    }
  })

  // A `not_installed` component used to grow a synthetic row here — the
  // release offers a plugin and nothing on this device declares it or has
  // its binary, so no line for it exists in `/api/status` at all. That row
  // shape now lives on its own screen (`InstallablesDialog.vue`, behind
  // "Add a component"): choosing to add something the device does not have
  // is a different question from managing what it runs, and mixing the two
  // in one table is what the owner objected to. `update.components` is
  // passed to the dialog directly, and its own `rows` computed does the
  // `not_installed` filtering — this table renders declared rows alone, and
  // `PluginRow` no longer carries a `not_installed` field at all: every row
  // this computed can ever produce is `declared`-or-`undeclared_binary`, so
  // a third, always-false flag would have been dead weight kept only for a
  // row shape that can no longer reach this table.
  //
  // The guard this used to carry — `!availableNames.has(c.name)`, excluding
  // a `not_installed` component already present as a declared row — is gone
  // with it, deliberately. Measured server-side
  // (`update::state`, `NotInstalled` is only ever set together with
  // `declared = false`): the two can never name the same component, so the
  // guard was only ever protecting row uniqueness inside this one table — a
  // concern that does not exist once "what you have" and "what you could
  // add" live on two different surfaces.
  return declaredRows
})

/** Position of every row that `plugins.toml` actually declares, among
 * themselves only: an `undeclared_binary` row never carries an arrow, so it
 * must not count when deciding which declared row sits at either end. */
const declaredOrder = computed(() => plugins.value.filter((p) => p.declared).map((p) => p.name))
const isFirstDeclared = (name: string) => declaredOrder.value[0] === name
const isLastDeclared = (name: string) =>
  declaredOrder.value[declaredOrder.value.length - 1] === name

/** The protocol this core speaks, as `/api/status` last reported it (loaded
 * alongside `status.value.plugins` — see `usePlugins`). The other half of the
 * sentence a refused plugin's badge writes: the line carries what its binary
 * announced, this carries what the core expects. */
const protocol = computed(() => status.value.protocol)

// Names of the plugins whose toggle is in flight: disabling the only source
// can cost up to 15 s (stop + Deactivate + Activate, each capped at 5 s) when
// the incoming or the outgoing one does not answer — precisely the textbook
// case that pushes one to disable a plugin (a `files` stuck on a dead share).
// Without this marker, the switch stayed clickable and the row looked inert
// during that whole window.
const inProgress = ref<Set<string>>(new Set())

async function togglePlugin(row: PluginRow) {
  if (inProgress.value.has(row.name)) return
  inProgress.value.add(row.name)
  try {
    const enable = row.disabled
    const err = await api.put(`/api/plugins/${encodeURIComponent(row.name)}/enabled`, {
      enabled: enable,
    })
    if (err) {
      toast.error(err)
    } else {
      toast.success(t.value(enable ? 'plugin_enabled' : 'plugin_disabled', { name: row.name }))
    }
    // Reload in both cases: a refusal may have left the previous state, and a
    // success changes the lines of several kinds at once.
    await loadAll()
  } finally {
    inProgress.value.delete(row.name)
  }
}

/**
 * One place a stale second tab can be told an arrow — or a drag — no longer
 * applies: `move_entry` refuses out of range rather than clamping, and this
 * refusal is reachable by an ordinary operator, and carries a catalog
 * sentence of its own (`plugin_move_out_of_range`). Surfaced exactly like any
 * other refusal here: read from the server's answer, never reworded on this
 * side.
 *
 * `to` is a target position among the **declared** plugins, not a step: a
 * drag can cross several ranks in one gesture, so the route learned a
 * position instead of the ±1 it used to accept. The arrows still call this
 * with their own neighbouring position (see `declaredIndex`) — one write
 * either way, immediate on drop rather than batched behind a save button,
 * which is fewer writes than the one-per-arrow-press this table already had.
 */
async function movePlugin(name: string, to: number) {
  // Fix round 1, M2: `inProgress` already exists for `togglePlugin`, and
  // every gesture this table added shares its row's name with that same
  // marker — a plugin mid-move is not a plugin that should also be toggled
  // or reinstalled from the very same row in the same instant.
  if (inProgress.value.has(name)) return
  inProgress.value.add(name)
  try {
    const err = await api.post(`/api/plugins/${encodeURIComponent(name)}/move`, { to })
    if (err) {
      toast.error(err)
      return
    }
    await refreshPlugins()
  } finally {
    inProgress.value.delete(name)
  }
}

/** This plugin's own position among the declared rows, or `-1` for a name
 * `declaredOrder` no longer carries — a stale drop target disappearing
 * mid-drag, say. Both the arrows and the drag handle compute a target
 * position from this rather than from the row's index in the full table,
 * which also lists rows nothing declares. */
function declaredIndex(name: string): number {
  return declaredOrder.value.indexOf(name)
}

/** Name of the plugin currently being dragged, or `null`: the row grays out
 * from this, same idiom as `RadioAdmin.vue`'s station table. */
const draggingPlugin = ref<string | null>(null)

/**
 * Drop handler for the plugins table's own drag handle.
 *
 * `to` is the row dropped onto's own position in `declaredOrder`, read
 * **before** the drop takes effect — the same convention `move()` itself
 * uses (splice the source out, then insert at `to` into what remains) and
 * the one `move_entry` already implements server-side. There is nothing
 * else to compute: this table has no local list to keep in step, since the
 * write lands immediately and the next `/api/status` poll is what redraws
 * the row order.
 */
async function dropPlugin(targetName: string) {
  const from = draggingPlugin.value
  draggingPlugin.value = null
  if (!from || from === targetName) return
  const to = declaredIndex(targetName)
  if (to === -1) return
  await movePlugin(from, to)
}

/**
 * Gets a component's binary onto the device: the release is downloaded,
 * placed, and — for a name `plugins.toml` does not yet declare — the archive's
 * own `[[plugin]]` fragment is appended first. That single worker path
 * (`Worker::install_one`) is what a `missing_binary` row's "Install" and an
 * `undeclared_binary` row's "Declare" both reduce to: the first has a
 * declaration and needs a binary, the second already has a binary and needs a
 * declaration, and either way `/api/update/install` is the one gesture that
 * can write it. Same async, poll-while-busy shape as `onConfirmInstall`.
 *
 * `inProgress` here only spans the enqueue request, not the background
 * install itself (that one is `update.busy`, the update card's own field,
 * covered by `pollUpdateWhileBusy`) — enough to stop the same row's button
 * being pressed twice in the same instant, though not to stop a second press
 * once the 202 has come back and the worker is still busy underneath it.
 */
async function installPlugin(name: string) {
  if (inProgress.value.has(name)) return
  inProgress.value.add(name)
  try {
    const err = await api.post('/api/update/install', { components: [name] })
    if (err) {
      toast.error(err)
      return
    }
    pollUpdateWhileBusy()
    await refreshUpdate()
  } finally {
    inProgress.value.delete(name)
  }
}

/** Name of the plugin an uninstall confirmation is open for, or `null` when
 * the dialog is closed. One name, not a `Set` like `inProgress`: only one
 * confirmation can be on screen at a time. */
const uninstallTarget = ref<string | null>(null)

async function confirmUninstall() {
  const name = uninstallTarget.value
  uninstallTarget.value = null
  if (!name || inProgress.value.has(name)) return
  inProgress.value.add(name)
  try {
    const err = await api.del(`/api/plugins/${encodeURIComponent(name)}`)
    if (err) {
      toast.error(err)
    } else {
      // Nothing richer than "OK": the sentence the operator actually needs —
      // that their stations survive — was already said in the confirmation
      // they just read (Ruling 73), not repeated here as a second, drifting
      // copy of it.
      toast.success(t.value('ok'))
    }
    await loadAll()
  } finally {
    inProgress.value.delete(name)
  }
}

/** File name a "remove the binary" confirmation is open for, or `null` when
 * the dialog is closed. Routed through the same `Dialog` pattern as
 * `uninstallTarget`, with its own sentence (fix round 1, M1): this is the one
 * gesture in this table that cannot be undone by writing the manifest back,
 * so it earns the confirmation Uninstall already had. */
const removeBinaryTarget = ref<string | null>(null)

/**
 * Erases a binary nothing declares — what a hand-dropped file or an
 * interrupted uninstall (declaration removed, the privileged unit never run)
 * leaves behind.
 *
 * `DELETE /api/plugins/binaries/{file}` (fix round 1, I1), never
 * `DELETE /api/plugins/{name}`: that route erases a **declaration** and
 * refuses a name it does not find in `plugins.toml`, which an
 * `undeclared_binary` row's name never is by definition. This one takes the
 * **file** name (`p.binary_file`), not the component name (`p.name`) — the
 * two differ once the release's own naming convention applies to the file,
 * and the server-side route addresses the plugins directory by file, not by
 * component.
 */
async function confirmRemoveBinary() {
  const file = removeBinaryTarget.value
  removeBinaryTarget.value = null
  if (!file || inProgress.value.has(file)) return
  inProgress.value.add(file)
  try {
    const err = await api.del(`/api/plugins/binaries/${encodeURIComponent(file)}`)
    if (err) {
      toast.error(err)
    } else {
      toast.success(t.value('ok'))
    }
    await loadAll()
  } finally {
    inProgress.value.delete(file)
  }
}

async function changeOutput() {
  const err = await api.put('/api/audio-output', {
    device: device.value === SYSTEM_DEFAULT ? null : device.value,
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

/**
 * The `/api/settings` body, shared by every card that writes it
 * (`saveSettings` and `saveDisplay`): a single conversion list rather than
 * two copies that would drift.
 */
function settingsPayload() {
  return {
    ...settings.value,
    volume_repeat_initial_ms: Number(settings.value.volume_repeat_initial_ms),
    volume_repeat_interval_ms: Number(settings.value.volume_repeat_interval_ms),
    overlay_ms: Number(settings.value.overlay_ms),
    tens_window_ms: Number(settings.value.tens_window_ms),
    seek_step_s: Number(settings.value.seek_step_s),
    // Both read from a plain number input (`Input` has no `.number` modifier
    // on its native `v-model`), so an edited field is a **string** here.
    // Uncast, a string fails the core's `u32` deserialization and refuses the
    // *whole* PUT — not just this field — the first time a user touches
    // either box, which is exactly the defect this cast closes.
    cover_cache_budget_mio: Number(settings.value.cover_cache_budget_mio),
    cover_download_max_mio: Number(settings.value.cover_download_max_mio),
    // The four rendition settings are sent **even when the switch is
    // unchecked**, and that is deliberate: the UI greys them out without
    // emptying them, so re-checking the switch finds the values that had been
    // set. Omitting them would drop them back to the core defaults (the struct
    // is `serde(default)`), i.e. silently lose a setting visible on screen.
    cover_source_max_mio: Number(settings.value.cover_source_max_mio),
    cover_max_edge_px: Number(settings.value.cover_max_edge_px),
    cover_jpeg_quality: Number(settings.value.cover_jpeg_quality),
    cover_passthrough_max_ko: Number(settings.value.cover_passthrough_max_ko),
    cover_max_pixels_mpx: Number(settings.value.cover_max_pixels_mpx),
    update_hour: Number(settings.value.update_hour),
  }
}

async function saveSettings() {
  const err = await api.put('/api/settings', settingsPayload())
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

/**
 * The two update gestures. Neither refreshes `/api/update` a single time
 * after its `POST` resolves: the worker acts asynchronously, so a lone
 * reload right after would very often still read the pre-gesture state —
 * see `pollUpdateWhileBusy`, which watches `busy` rather than a delay. The
 * repo has already paid for a `watch` that observed the wrong thing once.
 *
 * The route answers **202 on enqueue only** — `busy` is set afterwards, by
 * the worker task, not by this request. So the poll is armed
 * unconditionally on a successful 202, never gated on one immediate
 * snapshot: that snapshot can race the worker and read `busy: null` before
 * it has taken the write lock, which used to leave the card stale with its
 * buttons enabled until the operator pressed F5 — silently, exactly the
 * failure mode this product has already been bitten by once.
 */
let updatePoll: ReturnType<typeof setInterval> | null = null

function stopUpdatePoll() {
  if (updatePoll !== null) {
    clearInterval(updatePoll)
    updatePoll = null
  }
}

async function refreshUpdate() {
  update.value = await api.get<UpdatePayload>('/api/update').catch(() => update.value)
}

function pollUpdateWhileBusy() {
  stopUpdatePoll()
  updatePoll = setInterval(async () => {
    await refreshUpdate()
    if (!update.value.busy) stopUpdatePoll()
  }, 2000)
}

/**
 * Ceiling for `pollLanguageWhileBusy`, in ticks of its own 2 s interval — 20 s
 * total. Not a measured worst case: a language pack is a small, text-only
 * archive, and an ordinary install or removal settles in well under this.
 * The ceiling exists so the poll cannot run forever if a job never reaches a
 * terminal state at all (the worker restarting mid-job, say) — the same
 * "no route may block, no page may wait on one" rule that gives the admin
 * protocol its own 5 s deadline, applied here on the polling side instead.
 */
const MAX_LANGUAGE_POLL_ATTEMPTS = 10

let languagePoll: ReturnType<typeof setInterval> | null = null

function stopLanguagePoll() {
  if (languagePoll !== null) {
    clearInterval(languagePoll)
    languagePoll = null
  }
}

/**
 * Whether `packs` already reflects `busy`'s outcome.
 *
 * **Remove** keeps its own rule, unrelated to the install one below: the
 * row is gone entirely — the language was not offered by the release
 * either, so `language_pack_rows` (`status::locales`) has nothing left to
 * say about it — or present with `installed: null`.
 *
 * **Install compares against the value `installed` held the moment the
 * gesture started (`installedAtStart`), not against a fixed shape** (fix
 * round 3 of the review: F4). `LanguagePacksRow`'s "Install" and "Update"
 * both call `installLanguage`, i.e. both produce `busy.action === 'install'`
 * — but a row only ever offers "Update" when `installed` is *already*
 * non-null, so the round-2 rule (`row.installed !== null`) was satisfied on
 * the very first read for an update, before the reinstall had landed: it
 * was only ever correct for a fresh install, whose `installedAtStart` is
 * `null`. "The value changed since the click" is the one rule that covers
 * both without a special case — a fresh install moves from `null` to a
 * version, an update moves from the old version to the new one.
 *
 * **A reinstall of the identical version can never satisfy this rule.**
 * That is not a defect in the rule: it is an outcome this predicate cannot
 * observe by construction (the row looks the same before and after), and
 * the ceiling in `pollLanguageWhileBusy` is exactly the backstop for a
 * gesture whose completion is unobservable this way — not a sign that
 * something is broken when it fires for that case.
 */
function languageGestureSettled(
  packs: LanguagePackRow[],
  busy: LanguageBusy,
  installedAtStart: string | null,
): boolean {
  const row = packs.find((p) => p.language === busy.language)
  if (busy.action === 'remove') return !row || row.installed === null
  return !!row && row.installed !== installedAtStart
}

/**
 * Refreshes `/api/locale` (and, for good measure, `/api/update`) until
 * `LanguagePacksRow`'s own payload reflects what `busy` describes, or the
 * ceiling above is reached — whichever comes first. Either way, `packBusy`
 * clears here, not in `installLanguage`/`confirmRemoveLanguage`'s own
 * `finally`: the row keeps showing the in-flight verb, and its buttons stay
 * disabled, for the whole window the operator would otherwise see nothing
 * happen in.
 *
 * **Why not just extend `pollUpdateWhileBusy`.** That poll stops on
 * `!update.value.busy`, which does not track a language job the way it
 * tracks a component install: `Job::RemoveLanguage` never calls `set_busy`
 * at all (`remove_language`, `update/mod.rs`), and `Job::InstallLanguage`
 * sets it only for the brief `check()` call ahead of the download — neither
 * shape stays "busy" for as long as the pack actually takes to land or
 * leave. Polling the payload this row actually reads, against a completion
 * predicate this component can state precisely (`languageGestureSettled`),
 * is what proves the row is right, rather than hoping a signal built for a
 * different job shape happens to still be true.
 *
 * `installedAtStart` is read by the caller from `locale.value.packs` at the
 * moment the gesture is enqueued, before anything here can have changed it
 * — see `languageGestureSettled`'s own doc for why an install needs it and
 * a remove does not.
 */
function pollLanguageWhileBusy(busy: LanguageBusy, installedAtStart: string | null) {
  stopLanguagePoll()
  let attempts = 0
  languagePoll = setInterval(async () => {
    attempts += 1
    await refreshUpdate()
    locale.value = await api.get<LocalePayload>('/api/locale').catch(() => locale.value)
    if (
      languageGestureSettled(locale.value.packs, busy, installedAtStart)
      || attempts >= MAX_LANGUAGE_POLL_ATTEMPTS
    ) {
      stopLanguagePoll()
      packBusy.value = null
    }
  }, 2000)
}

async function onUpdateCheck() {
  // `api.post` never rejects: a network failure comes back as the error
  // string, exactly like a refused check would.
  const err = await api.post('/api/update/check', undefined)
  if (err) {
    toast.error(err)
    return
  }
  pollUpdateWhileBusy()
  await refreshUpdate()
}

const showInstallDialog = ref(false)
/** The installables dialog (`InstallablesDialog.vue`), behind its own
 * button: choosing to add a component the device does not have is a
 * different question from managing the ones it runs, so it does not share
 * `showInstallDialog`. */
const showInstallablesDialog = ref(false)

async function onConfirmInstall(names: string[]) {
  showInstallDialog.value = false
  const err = await api.post('/api/update/install', { components: names })
  if (err) {
    toast.error(err)
    return
  }
  pollUpdateWhileBusy()
  await refreshUpdate()
}

/**
 * Whether `code` is a language the payload's own `completeness` marks as
 * incomplete — the same predicate `LanguageCard` uses to decide whether to
 * show its fallback control, needed again here so `saveDisplay` submits
 * `fallback` under the exact condition the control was visible under. An
 * unknown code (no `completeness` entry at all) reads as complete: nothing
 * to send, same as `LanguageCard`'s own default.
 */
function localeIsIncomplete(code: string): boolean {
  return locale.value.completeness.some((c) => c.language === code && !c.complete)
}

/**
 * Saves the "Language and display" card: the ordinary settings first, the
 * language (and, when it applies, the fallback) only if either moved.
 *
 * **The order is load-bearing.** `PUT /api/locale` is followed by a full
 * reload of the page state (`loadAll`), which overwrites `settings` with what
 * the server holds. Sent before the settings `PUT`, it would therefore
 * discard every field of this card the owner had just edited — a date format
 * quietly reverting, with no error anywhere. A test pins the order.
 *
 * The language `PUT` is skipped when neither the language nor (for an
 * incomplete language) the fallback has moved: otherwise saving a date
 * format would reload the whole state for nothing, and the page would
 * flicker on an ordinary gesture.
 *
 * **`fallback` is submitted only when the chosen language is currently
 * incomplete** — exactly the condition under which `LanguageCard` shows the
 * control that edits it. Submitting it unconditionally would let a stale
 * value (kept around on purpose so the control does not forget it — see
 * `LanguageCard`'s own doc) overwrite the persisted fallback from a request
 * the owner never saw a control for; omitting the field on the wire leaves
 * the stored fallback untouched (`LocaleRequest.fallback`'s own contract on
 * the Rust side), which is exactly what "nothing to submit" must mean here.
 *
 * **And only when it actually moved.** The value in hand is what the last
 * `GET /api/locale` reported, and that response is *clamped*: a stored
 * fallback equal to the chosen language is reported as `"en"`, since it
 * resolves nothing (see `locale_json`'s own comment). Resubmitting an
 * untouched value would write that clamp back into `state.json` and destroy
 * a fallback the owner chose — exactly the loss the "its value stays
 * memorized" rule exists to prevent, arriving by the one door the rule did
 * not watch. Reading a value is not consenting to it: only a value the
 * owner edited is sent.
 *
 * Partial failure is a real state and is reported honestly: the first error
 * wins and the second write does not happen.
 */
async function saveDisplay() {
  const err = await api.put('/api/settings', settingsPayload())
  if (err) {
    toast.error(err)
    return
  }
  const incomplete = localeIsIncomplete(lang.value)
  const localeUnchanged =
    lang.value === loadedLocale.value && (!incomplete || fallback.value === loadedFallback.value)
  if (localeUnchanged) {
    toast.success(t.value('ok'))
    return
  }
  const body: { locale: string; fallback?: string } = { locale: lang.value }
  if (incomplete && fallback.value !== loadedFallback.value) body.fallback = fallback.value
  const localeErr = await api.put('/api/locale', body)
  if (localeErr) {
    toast.error(localeErr)
    return
  }
  await loadAll()
}

/**
 * The language gesture currently in flight — the language **and** which of
 * "install" or "remove" started it — or `null`. A single value, not a `Set`
 * like `inProgress`: `LanguagePacksRow` disables one row at a time, and only
 * one confirmation can be open at once (`removeLanguageTarget`, right
 * below).
 *
 * **Named here, not guessed downstream** (fix round 1, finding 3 of task
 * 10's review). `LanguagePacksRow` used to receive just the language and
 * infer the verb from `row.installed` — wrong today, not only in some
 * future refactor: a row that licenses both "Update" and "Remove" (a pack
 * already installed, with a newer one offered) keeps `row.installed`
 * non-null while an **install** (the reinstall-over-existing that "Update"
 * triggers) is in flight, so the old guess said "removing" for an install.
 * `ConfigView` already knows which button was pressed; it now says so.
 */
const packBusy = ref<LanguageBusy | null>(null)

/**
 * Installs every pack the release currently publishes for `language`, or
 * reinstalls it over an already-installed one that carries an older
 * version — `POST /api/languages/{language}` does not distinguish the two
 * (task 9's own route doc), so `LanguagePacksRow`'s "Install" and "Update"
 * buttons both call this.
 *
 * **Acknowledges success** (fix round 1, finding 2): the route answers 202
 * on enqueue only, so a message claiming the pack is installed would be
 * false — `confirmUninstall`'s "OK" is honest for its own route (a bare,
 * synchronous 204), but copying it here would claim a completion that has
 * not happened yet. Reusing `language_pack_installing` — the same sentence
 * `LanguagePacksRow` shows next to the busy row — states only what is true
 * at this point: the gesture was accepted and is under way.
 *
 * **`packBusy` outlives this function on the success path** (fix round 2,
 * the reviewer's own follow-up on finding 2): it used to clear in a
 * `finally` here, which released the row after the enqueue round trip alone
 * — long before the worker had actually installed anything, and nothing
 * afterwards ever told the row to look again. `pollLanguageWhileBusy` is
 * what clears it now, once the row's own payload says the pack really
 * landed (or the poll's ceiling gives up). On a refusal, though, there is
 * nothing to wait for, so `packBusy` is released immediately, right here.
 *
 * **Captures `installedAtStart` before the request goes out** (fix round 3,
 * F4): `languageGestureSettled` needs the value `installed` held at the
 * moment of the click, not a fixed shape — a row already installed is
 * exactly the "Update" case, and comparing against `null` would have
 * declared it settled before the reinstall had even begun.
 */
async function installLanguage(language: string) {
  if (packBusy.value) return
  const busy: LanguageBusy = { language, action: 'install' }
  const installedAtStart = locale.value.packs.find((p) => p.language === language)?.installed ?? null
  packBusy.value = busy
  const err = await api.post(`/api/languages/${encodeURIComponent(language)}`, {})
  if (err) {
    toast.error(err)
    packBusy.value = null
    return
  }
  toast.success(t.value('language_pack_installing', { language: languageName(language) }))
  pollLanguageWhileBusy(busy, installedAtStart)
  await loadAll()
}

/** Language a remove confirmation is open for, or `null` when the dialog is
 * closed — same shared-dialog pattern as `uninstallTarget`. */
const removeLanguageTarget = ref<string | null>(null)

/**
 * Opens the remove confirmation for `language` — never straight to
 * `DELETE /api/languages/{language}`: the sentence read there
 * (`language_pack_remove_confirm`) is the one place the owner learns the
 * interface itself may fall back to English if it is the language in use.
 */
function askRemoveLanguage(language: string) {
  if (packBusy.value) return
  removeLanguageTarget.value = language
}

/**
 * Retires every pack installed for the confirmed language. Same
 * confirmation idiom as `confirmUninstall` — except for the success
 * message, which cannot honestly be `confirmUninstall`'s "OK": this route,
 * like `installLanguage`'s, only enqueues (202), it does not report the
 * pack gone. `language_pack_removing` says what is actually true right now.
 *
 * **`packBusy` outlives this function on the success path**, exactly like
 * `installLanguage` — see that function's own doc. `Job::RemoveLanguage`
 * never even sets the server's own `busy` field (`remove_language`,
 * `update/mod.rs`, calls no `set_busy`), which is precisely why
 * `pollLanguageWhileBusy` watches this row's own payload instead of that
 * signal.
 */
async function confirmRemoveLanguage() {
  const language = removeLanguageTarget.value
  removeLanguageTarget.value = null
  if (!language || packBusy.value) return
  const busy: LanguageBusy = { language, action: 'remove' }
  packBusy.value = busy
  const err = await api.del(`/api/languages/${encodeURIComponent(language)}`)
  if (err) {
    toast.error(err)
    packBusy.value = null
    return
  }
  toast.success(t.value('language_pack_removing', { language: languageName(language) }))
  // `null`: `languageGestureSettled`'s `remove` branch never reads this
  // parameter, it only exists for the `install` branch (see its own doc).
  pollLanguageWhileBusy(busy, null)
  await loadAll()
}

/**
 * The table of contents: one entry per card, in template order. It is data
 * (like REMOTE_ROWS for the remote control): the view walks it for the nav AND
 * for the scroll observation.
 */
const SECTIONS = [
  { id: 'update', key: 'update_title' },
  { id: 'plugins', key: 'plugins_title' },
  { id: 'audio', key: 'audio_output' },
  { id: 'display', key: 'display_card_title' },
  { id: 'startup', key: 'startup_title' },
  { id: 'player', key: 'player_card_title' },
  { id: 'covers', key: 'cover_card_title' },
] as const

const active = ref<string>(SECTIONS[0].id)
// Visibility per section, kept up to date by the observer: the active section
// is the first visible one in table-of-contents order (not the last entry
// received, which depends on the arrival order of the callbacks).
const visible = new Set<string>()
let observer: IntersectionObserver | null = null

onMounted(() => {
  observer = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) visible.add(e.target.id)
        else visible.delete(e.target.id)
      }
      const first = SECTIONS.find((s) => visible.has(s.id))
      if (first) active.value = first.id
    },
    // The observation band is the top of the screen: the "active" section is
    // the one being read, not the one peeking in at the bottom.
    { rootMargin: '0px 0px -60% 0px' },
  )
  for (const s of SECTIONS) {
    const el = document.getElementById(s.id)
    if (el) observer.observe(el)
  }
})
onUnmounted(() => {
  observer?.disconnect()
  stopUpdatePoll()
  stopLanguagePoll()
})

function goTo(id: string) {
  active.value = id
  document.getElementById(id)?.scrollIntoView({ behavior: 'smooth' })
}
</script>

<template>
  <div class="flex gap-8">
    <div class="min-w-0 flex-1 space-y-4">
      <!-- Above the plugins table, not inside it (decision 8: the whole of
           auto-update lives on this one tab). The automatic-checks policy
           lives inside `UpdateCard` itself, below a separator: above it, two
           buttons — Check, Install — that act at once; below it, settings
           that wait for one Save button. That is one save path, not two, so
           merging what used to be a second card here into `UpdateCard` does
           not repeat the "two save paths behind one title" mistake this
           section once refused — there is only one. -->
      <section id="update" class="scroll-mt-6 space-y-4">
        <UpdateCard
          :update="update"
          :settings="settings"
          @check="onUpdateCheck"
          @install="showInstallDialog = true"
          @save="saveSettings"
        />

        <UpdateDialog
          :open="showInstallDialog"
          :components="update.components"
          @update:open="(v: boolean) => (showInstallDialog = v)"
          @confirm="onConfirmInstall"
        />
      </section>

      <section id="plugins" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('plugins_title') }}</CardTitle></CardHeader>
          <CardContent>
            <table class="w-full text-sm" data-plugins-table>
              <thead class="text-muted-foreground">
                <tr>
                  <th class="text-left font-normal">{{ t('col_plugin') }}</th>
                  <th class="text-left font-normal">{{ t('col_kind') }}</th>
                  <th class="text-left font-normal">{{ t('col_version') }}</th>
                  <th class="text-left font-normal">{{ t('col_state') }}</th>
                  <th class="text-left font-normal">{{ t('col_admin') }}</th>
                  <th class="text-left font-normal">{{ t('col_enabled') }}</th>
                  <th class="text-left font-normal">{{ t('col_order') }}</th>
                  <th class="text-left font-normal">{{ t('col_actions') }}</th>
                </tr>
              </thead>
              <tbody>
                <!--
                  Draggable rows: only a declared row has a `plugins.toml`
                  position to drop into, so the handle and the drag
                  attributes are conditional on `p.declared` — an
                  `undeclared_binary` or `missing_binary` row's arrows are
                  already hidden the same way. `dragover.prevent` is
                  essential, or the browser refuses the drop. The handle
                  lives inside the name cell rather than a column of its
                  own, so the header count this table is locked to in the
                  e2e journey does not change.
                -->
                <tr
                  v-for="p in plugins"
                  :key="p.name"
                  data-plugin-row
                  class="border-t border-border"
                  :class="p.declared && draggingPlugin === p.name ? 'opacity-50' : ''"
                  :draggable="p.declared"
                  @dragstart="p.declared && (draggingPlugin = p.name)"
                  @dragover.prevent
                  @drop.prevent="p.declared && dropPlugin(p.name)"
                  @dragend="draggingPlugin = null"
                >
                  <td class="py-1">
                    <!-- The handle sits beside the name, not inside
                         `[data-plugin-name]`: that attribute is the name's
                         own text everywhere else it is queried, and must
                         keep meaning only that. `aria-hidden` for the same
                         reason at the accessibility layer: the cell's own
                         accessible name (what the config journey looks the
                         row up by) must stay the plugin's name alone —
                         dragging is decorative and keyboard-inaccessible
                         either way, the arrows are the real affordance for
                         anyone not using a pointer. -->
                    <span
                      v-if="p.declared"
                      class="cursor-grab select-none pr-1"
                      :title="t('reorder_hint')"
                      aria-hidden="true"
                      data-plugin-drag-handle
                    >⠿</span>
                    <span data-plugin-name>{{ p.name }}</span>
                  </td>
                  <td data-plugin-kind>{{ p.kinds }}</td>
                  <td data-plugin-version>{{ p.version ?? '—' }}</td>
                  <td data-plugin-state>
                    <Badge
                      :variant="
                        p.incompatible !== undefined
                          ? 'destructive'
                          : p.missing_binary
                            ? 'outline'
                            : p.undeclared_binary
                              ? 'outline'
                              : p.disabled
                                ? 'outline'
                                : p.busy
                                  ? 'outline'
                                  : p.catalog_unknown && p.connected
                                    ? 'outline'
                                    : p.connected
                                      ? 'secondary'
                                      : p.starting
                                        ? 'secondary'
                                        : p.stalled
                                          ? 'outline'
                                          : 'destructive'
                      "
                    >
                      <!-- "Incompatible" comes **first**: a refused plugin is
                           neither connected, nor busy, nor merely silent, and
                           any other position would describe it with a word
                           that is false. `missing_binary` (declared, no
                           binary) and `undeclared_binary` (binary, no
                           declaration) come next, **before** "connected": both
                           are more precise than a bare "not connected", and
                           must not be confused with each other — they license
                           opposite gestures. "Busy" comes **before**
                           "connected": a busy plugin is reachable, and that is
                           precisely why "connected" says nothing useful.
                           `catalog_unknown` sits right after, but **paired
                           with `p.connected`** — unlike every flag above it,
                           this one is not exclusive with a connection
                           outcome either way: the core sets it from the
                           announcement alone, at the same site as
                           `ui_version`/`version`/`repository`, whether or
                           not the socket connect that follows succeeds (see
                           `main.rs`'s per-kind `Ok`/`Err` branches). Without
                           the `&& p.connected` guard a legacy plugin whose
                           socket had failed read this sentence instead of
                           "unavailable" below — the wrong cause, and the
                           more urgent fact suppressed (a defect this repo's
                           review caught with a probe test, not by reading
                           the code). This only names a binary built before
                           this core could ask it for its language packs
                           (the accepted mitigation for `PROTOCOL_VERSION`
                           staying at 1, see `Announcement.catalog`'s own
                           doc) — so it must not outrank a real fault like
                           "busy", but among **connected** plugins it must
                           still outrank a bare "connected", which would say
                           nothing about the missing translations.
                           "Starting" comes **before** "stalled": both say the
                           plugin has not spoken yet, and only the elapsed time
                           tells them apart. Showing "stalled" during a normal
                           startup wrongly accused a perfectly healthy binary.

                           `!== undefined` and not a truthiness test, in both
                           chains: `"protocol":0` deserializes perfectly well
                           and the serde default only applies when the key is
                           *absent*, so a refusal at protocol 0 would read as
                           "no refusal" and be shown as merely unavailable.
                           The accumulator already takes that care with `??`;
                           testing `p.incompatible` here would undo it.

                           `update_binary_missing`/`update_undeclared` are the
                           same catalog keys `/api/update`'s own card would use
                           for the matching `Availability` — one wording per
                           condition, never reinvented here, so the table and
                           the update card can never disagree about what to
                           call it. -->
                      {{
                        p.incompatible !== undefined
                          ? t('plugin_incompatible', { found: p.incompatible, expected: protocol })
                          : p.missing_binary
                            ? t('update_binary_missing')
                            : p.undeclared_binary
                              ? t(p.removal_pending ? 'update_removal_pending' : 'update_undeclared')
                              : p.disabled
                                ? t('disabled')
                                : p.busy
                                  ? t('busy')
                                  : p.catalog_unknown && p.connected
                                    ? t('plugin_catalog_unknown')
                                    : p.connected
                                      ? t('connected')
                                    : p.starting
                                      ? t('starting')
                                      : p.stalled
                                        ? t('stalled')
                                        : t('unavailable')
                      }}
                    </Badge>
                  </td>
                  <td>
                    <RouterLink v-if="p.admin" :to="`/plugins/${p.name}/`" data-admin-link class="underline">
                      {{ t('admin_link') }}
                    </RouterLink>
                    <span v-else>-</span>
                  </td>
                  <td>
                    <!-- No confirmation: the action is reversible from this
                         same row, and the notification says what happened.
                         Only a declared row has anything to enable or
                         disable: an `undeclared_binary` row carries no
                         manifest entry for the switch to flip. -->
                    <Switch
                      v-if="p.declared"
                      data-plugin-toggle
                      :model-value="!p.disabled"
                      :disabled="inProgress.has(p.name)"
                      :aria-label="t('toggle_plugin', { name: p.name })"
                      @click="togglePlugin(p)"
                    />
                    <span v-else>-</span>
                  </td>
                  <td data-plugin-order>
                    <!-- Arrows write `/etc/ritornello/plugins.toml` and only
                         a truly declared name has a line in it for
                         `move_entry` to act on. Disabled, not hidden, at
                         either end: `move_entry` refuses out of range rather
                         than clamping, and an arrow that can be pressed and
                         always fails is worse than a greyed one. -->
                    <div v-if="p.declared" class="flex gap-1">
                      <!-- Alternative to the drag handle: neither the
                           keyboard nor a touchscreen fares well with
                           drag-and-drop, the same reasoning the radio
                           station table already carries. -->
                      <Button
                        variant="ghost" size="icon" data-plugin-up
                        :disabled="isFirstDeclared(p.name) || inProgress.has(p.name)"
                        :aria-label="t('plugin_move_up')"
                        @click="movePlugin(p.name, declaredIndex(p.name) - 1)"
                      >▲</Button>
                      <Button
                        variant="ghost" size="icon" data-plugin-down
                        :disabled="isLastDeclared(p.name) || inProgress.has(p.name)"
                        :aria-label="t('plugin_move_down')"
                        @click="movePlugin(p.name, declaredIndex(p.name) + 1)"
                      >▼</Button>
                    </div>
                    <span v-else>-</span>
                  </td>
                  <td data-plugin-actions>
                    <!-- Two states carry two gestures each, and never the same
                         pair twice (Ruling 13/65): `missing_binary` (declared,
                         no binary) installs or uninstalls; `undeclared_binary`
                         (binary, no declaration) declares or removes the
                         binary. Every other row already has its binary and
                         its declaration, so only uninstalling applies. A
                         `not_installed` row (neither binary nor declaration,
                         offered by the release) used to be a third state
                         here with only Install to offer; that row shape now
                         lives in `InstallablesDialog.vue` instead, so this
                         table never renders it any more. -->
                    <!-- Ruling 88, applied here for the same reason it
                         applies to `UpdateDialog`'s switch: `offered ===
                         null` (never `kind`) is the guard, and it is
                         kind-agnostic on purpose — a `missing_binary` row the
                         release does not currently carry, or an
                         `undeclared_binary` row with nothing to declare it
                         from, must not offer a button that can only fail. -->
                    <div class="flex gap-1">
                      <Button
                        v-if="p.missing_binary"
                        variant="outline" size="xs" data-plugin-install
                        :disabled="p.offered === null || inProgress.has(p.name)"
                        @click="installPlugin(p.name)"
                      >{{ t('plugin_install') }}</Button>
                      <!-- Both gestures this state licenses are withheld while
                           the binary is already being erased: declaring a file
                           that is about to vanish, or asking a second time for
                           the erasure in flight, are the two ways this row used
                           to mislead. The row says what is happening instead. -->
                      <Button
                        v-if="p.undeclared_binary && !p.removal_pending"
                        variant="outline" size="xs" data-plugin-declare
                        :disabled="p.offered === null || inProgress.has(p.name)"
                        @click="installPlugin(p.name)"
                      >{{ t('plugin_declare') }}</Button>
                      <Button
                        v-if="p.undeclared_binary && !p.removal_pending"
                        variant="outline" size="xs" data-plugin-remove-binary
                        @click="removeBinaryTarget = p.binary_file ?? p.name"
                      >{{ t('plugin_remove_binary') }}</Button>
                      <Button
                        v-if="!p.undeclared_binary"
                        variant="outline" size="xs" data-plugin-uninstall
                        @click="uninstallTarget = p.name"
                      >{{ t('plugin_uninstall') }}</Button>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
            <p class="mt-2 text-xs text-muted-foreground">{{ t('plugin_order_note') }}</p>
            <!-- Its own screen, behind its own button: choosing to ADD a
                 component the device does not have is a different question
                 from managing the ones it runs, so `not_installed` rows no
                 longer sit in the table above (see the `plugins` computed). -->
            <Button
              variant="outline" size="sm" class="mt-3" data-installables-open
              @click="showInstallablesDialog = true"
            >{{ t('installables_title') }}</Button>
          </CardContent>
        </Card>

        <InstallablesDialog
          :open="showInstallablesDialog"
          :components="update.components"
          :outcome="update.outcome"
          :last-check-unix-s="update.last_check_unix_s"
          :busy="update.busy"
          @update:open="(v: boolean) => (showInstallablesDialog = v)"
          @install="installPlugin"
        />

        <!-- One shared dialog for the whole table, keyed by `uninstallTarget`
             rather than one per row: only one confirmation is ever on screen,
             and `Dialog` stays mounted between rows the same way it does
             between openings elsewhere on this page (see `UpdateDialog`). -->
        <Dialog
          :open="uninstallTarget !== null"
          @update:open="(v: boolean) => { if (!v) uninstallTarget = null }"
        >
          <DialogContent data-plugin-uninstall-dialog>
            <DialogHeader>
              <DialogTitle>{{ t('plugin_uninstall') }}</DialogTitle>
              <!-- The one place the operator learns their stations survive
                   (Ruling 13/73): said here, before the gesture, not echoed
                   back afterwards by a second, drifting copy of the same
                   sentence. -->
              <DialogDescription>
                {{ uninstallTarget ? t('plugin_uninstall_confirm', { name: uninstallTarget }) : '' }}
              </DialogDescription>
            </DialogHeader>
            <Button
              variant="destructive" data-plugin-uninstall-confirm
              :disabled="uninstallTarget !== null && inProgress.has(uninstallTarget)"
              @click="confirmUninstall"
            >
              {{ t('plugin_uninstall') }}
            </Button>
          </DialogContent>
        </Dialog>

        <!-- Fix round 1, M1: erasing a binary is the one gesture here that
             cannot be undone by writing the manifest back (Uninstall, by
             contrast, is one Install away from reversed), and it did not
             have a confirmation of its own until the backend that makes it
             actually succeed existed (I1). Same shared-dialog pattern, its
             own sentence. -->
        <Dialog
          :open="removeBinaryTarget !== null"
          @update:open="(v: boolean) => { if (!v) removeBinaryTarget = null }"
        >
          <DialogContent data-plugin-remove-binary-dialog>
            <DialogHeader>
              <DialogTitle>{{ t('plugin_remove_binary') }}</DialogTitle>
              <DialogDescription>
                {{ removeBinaryTarget ? t('plugin_remove_binary_confirm', { file: removeBinaryTarget }) : '' }}
              </DialogDescription>
            </DialogHeader>
            <Button
              variant="destructive" data-plugin-remove-binary-confirm
              :disabled="removeBinaryTarget !== null && inProgress.has(removeBinaryTarget)"
              @click="confirmRemoveBinary"
            >
              {{ t('plugin_remove_binary') }}
            </Button>
          </DialogContent>
        </Dialog>
      </section>

      <section id="audio" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('audio_output') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <!-- The card title is not associated with the trigger: without an
                 aria-label, the selector has no accessible name at all. -->
            <Select v-model="device">
              <SelectTrigger class="min-w-64" :aria-label="t('audio_output')"><SelectValue>{{ deviceLabel }}</SelectValue></SelectTrigger>
              <SelectContent>
                <SelectItem :value="SYSTEM_DEFAULT" data-audio-default>
                  {{ t('audio_default_device') }}
                </SelectItem>
                <!-- Readable description as primary, technical name as
                     secondary — same pattern as "Français" shown / `fr` sent
                     for the languages. -->
                <SelectItem v-for="d in devices" :key="d.name" :value="d.name">
                  <div class="flex flex-col items-start">
                    <span>{{ d.description || d.name }}</span>
                    <span v-if="d.description" class="text-xs text-muted-foreground">{{ d.name }}</span>
                  </div>
                </SelectItem>
              </SelectContent>
            </Select>
            <Button data-audio-change :disabled="audioUnavailable" @click="changeOutput">{{ t('save') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Language and display: the owner's own three pairings ("language
           goes with date/time", "date/time is display", "overlays are
           display") only resolve into distinct cards if read in isolation —
           together they name one card, not two. One button, because the two
           routes behind it interact: see `saveDisplay`. -->
      <section id="display" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('display_card_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-col gap-4">
            <LanguageCard
              :payload="locale"
              :lang="lang"
              :fallback="fallback"
              @update:lang="(v) => (lang = v)"
              @update:fallback="(v) => (fallback = v)"
            />

            <LanguagePacksRow
              :payload="locale"
              :busy="packBusy"
              @install="installLanguage"
              @remove="askRemoveLanguage"
            />

            <!-- Date and time. Two separate settings, at the owner's request: the
                 order of a date and the 12/24 h format do not vary together from one
                 country to another. No time zone setting — the display runs on the
                 device, the page formats in the browser's time zone, and a third
                 setting could only contradict one of the two. -->
            <div class="flex flex-wrap items-end gap-4 border-t border-border pt-4">
              <label class="grid gap-1 text-sm">
                {{ t('clock_date_label') }}
                <Select v-model="settings.date_format">
                  <SelectTrigger class="min-w-36" data-date-format-select :aria-label="t('clock_date_label')"><SelectValue>{{ dateFormatLabel }}</SelectValue></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="day_month_year">{{ t('clock_date_dmy') }}</SelectItem>
                    <SelectItem value="year_month_day">{{ t('clock_date_ymd') }}</SelectItem>
                    <SelectItem value="month_day_year">{{ t('clock_date_mdy') }}</SelectItem>
                  </SelectContent>
                </Select>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('clock_hours_label') }}
                <!-- A boolean rendered as two named choices rather than a
                     checkbox: "24 h" is not the absence of "12 h", and a checkbox
                     labelled "24 h" would read badly when unchecked. -->
                <Select :model-value="settings.clock_24h ? '24' : '12'"
                        @update:model-value="(v) => (settings.clock_24h = v === '24')">
                  <SelectTrigger class="min-w-36" data-clock-hours-select :aria-label="t('clock_hours_label')"><SelectValue>{{ clockHoursLabel }}</SelectValue></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="24">{{ t('clock_24h') }}</SelectItem>
                    <SelectItem value="12">{{ t('clock_12h') }}</SelectItem>
                  </SelectContent>
                </Select>
              </label>
              <p class="w-full text-sm text-muted-foreground">{{ t('clock_hint') }}</p>
            </div>

            <div class="flex flex-wrap items-end gap-4 border-t border-border pt-4">
              <label class="grid gap-1 text-sm">
                {{ t('overlay_ms_label') }}
                <Input type="number" min="1000" max="15000" step="500" class="w-28" data-overlay-ms
                  v-model="settings.overlay_ms" />
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('tens_window_ms_label') }}
                <Input type="number" min="1000" max="15000" step="500" class="w-28" data-tens-window-ms
                  v-model="settings.tens_window_ms" />
              </label>
            </div>

            <Button data-display-change @click="saveDisplay">{{ t('save') }}</Button>
          </CardContent>
        </Card>

        <!-- Same shared-dialog pattern as `uninstallTarget`: only one
             confirmation is ever open at a time. The sentence
             (`language_pack_remove_confirm`) is the one place the owner
             learns the interface itself may fall back to English. -->
        <Dialog
          :open="removeLanguageTarget !== null"
          @update:open="(v: boolean) => { if (!v) removeLanguageTarget = null }"
        >
          <DialogContent data-language-pack-remove-dialog>
            <DialogHeader>
              <DialogTitle>{{ t('language_pack_remove') }}</DialogTitle>
              <DialogDescription>
                {{
                  removeLanguageTarget
                    ? t('language_pack_remove_confirm', { language: languageName(removeLanguageTarget) })
                    : ''
                }}
              </DialogDescription>
            </DialogHeader>
            <Button
              variant="destructive" data-language-pack-remove-confirm
              :disabled="removeLanguageTarget !== null && packBusy?.language === removeLanguageTarget"
              @click="confirmRemoveLanguage"
            >
              {{ t('language_pack_remove') }}
            </Button>
          </DialogContent>
        </Dialog>
      </section>

      <section id="startup" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('startup_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <Select v-model="settings.startup_power">
              <SelectTrigger class="min-w-32" data-startup-select :aria-label="t('startup_title')"><SelectValue>{{ startupLabel }}</SelectValue></SelectTrigger>
              <SelectContent>
                <SelectItem value="on">{{ t('startup_on') }}</SelectItem>
                <SelectItem value="standby">{{ t('startup_standby') }}</SelectItem>
                <SelectItem value="previous">{{ t('startup_previous') }}</SelectItem>
              </SelectContent>
            </Select>
            <Button data-startup-change @click="saveSettings">{{ t('save') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Player: the owner's own pairing, "volume hold and seeking are
           player configuration". One button: both fields already share the
           same route (`saveSettings`), so merging them costs nothing beyond
           the card boundary. -->
      <section id="player" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('player_card_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-col gap-4">
            <div class="flex flex-wrap items-end gap-4">
              <label class="grid gap-1 text-sm">
                {{ t('volume_hold_initial') }}
                <Input type="number" min="200" max="5000" step="100" class="w-28" data-hold-initial
                  v-model="settings.volume_repeat_initial_ms" />
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('volume_hold_interval') }}
                <Input type="number" min="100" max="2000" step="50" class="w-28" data-hold-interval
                  v-model="settings.volume_repeat_interval_ms" />
              </label>
              <p data-hold-hint class="w-full text-sm text-muted-foreground">{{ t('volume_hold_hint') }}</p>
            </div>

            <div class="flex flex-wrap items-end gap-4 border-t border-border pt-4">
              <label class="grid gap-1 text-sm">
                {{ t('seek_step_label') }}
                <Input type="number" min="1" max="120" class="w-28" data-seek-step-s
                  v-model="settings.seek_step_s" />
              </label>
            </div>

            <Button data-player-change @click="saveSettings">{{ t('save') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Covers. One card, one subject: everything the appliance applies to
           an album cover, in the order a reader meets it — what the cache may
           hold, the two ceilings on what may be read, then the re-encoding
           switch and the four settings that describe nothing but the
           thumbnail, its predicted weight among them. The estimate concludes
           the card rather than sitting mid-card: it depends on nearly every
           setting above it (the switch included), so naming its inputs only
           makes sense once they have all been read.

           These were briefly two cards, "what is kept in memory" and "what is
           read to publish". The distinction they drew is real and the field
           help still carries it, but as card titles they read as two
           unrelated rubrics, and they left the table of contents announcing a
           heading the page no longer had.

           `cover_source_max_mio` sits above the rule, among the ceilings: it
           applies whatever happens, and is the only guard left once
           re-encoding is unchecked.

           Greyed out, not emptied: the four rendition settings stay readable
           and go back in the PUT (see `saveSettings`), so re-checking the
           switch finds what had been set. -->
      <section id="covers" class="scroll-mt-6">
        <Card>
          <CardHeader>
            <!-- Same shape as the Memory card on the System tab: a title
                 holding a second, lighter element beside it. `items-center`
                 and not `items-baseline` — a flex container ignores the
                 `align-middle` `HelpButton` carries for inline use, so a
                 24 px box only reads level if centred from here.
                 **The `(?)` belongs to the card, not to the estimate.** It
                 used to sit at the foot of the card beside the estimate,
                 which read as a footnote on that one sentence; what it opens
                 is the state of the whole cache this card configures. -->
            <CardTitle class="flex items-center gap-1.5">
              {{ t('cover_card_title') }}
              <CoverCacheDetails />
            </CardTitle>
          </CardHeader>
          <CardContent class="space-y-4">
            <label class="grid gap-1 text-sm">
              {{ t('cover_cache_budget_label') }}
              <Input type="number" min="8" max="256" step="1" class="w-28" data-cover-cache-budget
                v-model="settings.cover_cache_budget_mio" />
              <span class="max-w-md text-xs text-muted-foreground">{{ t('cover_cache_budget_help') }}</span>
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('cover_download_max_label') }}
              <Input type="number" min="1" max="20" class="w-28" data-cover-download-max
                v-model="settings.cover_download_max_mio" />
              <span class="max-w-md text-xs text-muted-foreground">{{ t('cover_download_max_help') }}</span>
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('cover_source_max_label') }}
              <Input type="number" min="1" max="20" class="w-28" data-cover-source-max
                v-model="settings.cover_source_max_mio" />
              <span class="max-w-md text-xs text-muted-foreground">{{ t('cover_source_max_help') }}</span>
            </label>

            <div class="border-t border-border pt-4">
              <label class="flex items-start gap-3 text-sm">
                <Switch
                  data-cover-rendition
                  :model-value="settings.cover_rendition"
                  @update:model-value="(v: boolean) => (settings.cover_rendition = v)"
                />
                <span class="grid gap-1">
                  {{ t('cover_rendition_label') }}
                  <span class="text-xs text-muted-foreground">{{ t('cover_rendition_help') }}</span>
                </span>
              </label>
            </div>

            <!-- `aria-disabled` on top of each field's `disabled`: the whole
                 group is inactive, and a screen reader must be able to announce
                 it once rather than field by field.

                 The threshold comes first: it conditions what follows (does
                 the pipeline even reach the edge and the quality?), and the
                 real order in cover.rs confirms it — the pixel guard, then the
                 pass-through, then the encode. -->
            <div
              data-cover-rendition-group
              :aria-disabled="!settings.cover_rendition"
              :class="['flex flex-wrap items-start gap-4', settings.cover_rendition ? '' : 'opacity-50']"
            >
              <label class="grid gap-1 text-sm">
                {{ t('cover_passthrough_max_label') }}
                <Input type="number" min="16" max="2048" class="w-28" data-cover-passthrough-max
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_passthrough_max_ko" />
                <span class="text-xs text-muted-foreground">{{ t('cover_passthrough_max_help') }}</span>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_edge_label') }}
                <Input type="number" min="64" max="2048" class="w-28" data-cover-max-edge
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_edge_px" />
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_jpeg_quality_label') }}
                <Input type="number" min="40" max="100" class="w-28" data-cover-jpeg-quality
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_jpeg_quality" />
                <span class="text-xs text-muted-foreground">{{ t('cover_jpeg_quality_help') }}</span>
              </label>
              <!-- Placed right after the quality, in the greyed group: it
                   describes nothing but the thumbnail, so it lives among the
                   settings that decide it, not near the budget it later
                   feeds. -->
              <p
                v-if="coverPredictedText"
                class="max-w-md text-xs text-muted-foreground"
                data-cover-predicted-weight
              >
                {{ coverPredictedText }}
              </p>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_pixels_label') }}
                <Input type="number" min="1" max="64" class="w-28" data-cover-max-pixels
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_pixels_mpx" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_pixels_help') }}</span>
              </label>
            </div>

            <!-- The live estimate concludes the card, not the greyed group
                 above: it now depends on nearly every setting on it (budget,
                 download ceiling, edge, quality, threshold, and the switch
                 itself), and it must read whether re-encoding is on or off —
                 hence not greyed. -->
            <div class="border-t border-border pt-4">
              <p class="max-w-md text-xs text-muted-foreground" data-cover-cache-estimate>
                {{ coverCacheEstimateText }}
              </p>
            </div>

            <Button data-cover-change @click="saveSettings">{{ t('save') }}</Button>
          </CardContent>
        </Card>
      </section>
    </div>

    <nav data-toc :aria-label="t('toc_label')" class="sticky top-6 hidden w-40 shrink-0 self-start lg:block">
      <ul class="space-y-1 text-sm">
        <li v-for="s in SECTIONS" :key="s.id">
          <a
            :href="`#${s.id}`"
            data-toc-link
            :aria-current="active === s.id ? 'true' : undefined"
            :class="active === s.id ? 'font-medium text-foreground' : 'text-muted-foreground'"
            @click.prevent="goTo(s.id)"
          >
            {{ t(s.key) }}
          </a>
        </li>
      </ul>
    </nav>
  </div>
</template>
