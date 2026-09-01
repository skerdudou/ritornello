<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle, Input,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import { usePlugins } from '../composables/usePlugins'
import type { AudioPayload, LocalePayload, SettingsPayload } from '../types'

const { t, reload } = useCatalog()
// The plugin state comes from the module, not from a local `ref`: the top
// navigation reads the **same** object, so a toggle made here updates its menu
// without a reload. See `usePlugins`.
const { state: status, refresh: refreshPlugins } = usePlugins()
const audio = ref<AudioPayload>({ devices: [], current: null })
const locale = ref<LocalePayload>({ locales: [], current: null })
const device = ref('')
const lang = ref('')
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
  cover_max_bytes_ko: 512,
  cover_max_pixels_mpx: 16,
})

/**
 * Cap of a **network** cover in memory, in mebibytes.
 *
 * Now a setting in its own right (`cover_download_max_mio`) rather than a
 * copy of a core constant: the estimate below reads the same figure the
 * download itself is cut at, so the two can no longer silently diverge.
 *
 * **Minimal wiring, not the redesigned panel** — the two-box layout and the
 * live floor/typical estimate described in the design belong to the task
 * that rebuilds this panel; this computed only keeps the existing line
 * truthful now that the settings it reads have changed shape.
 */
const capPerCover = computed(() =>
  Math.min(
    Number(settings.value.cover_download_max_mio) || 2,
    Number(settings.value.cover_source_max_mio) || 2,
  ),
)

/**
 * The memory budget, in mebibytes, as entered by the user.
 *
 * Used to read `entries * capPerCover`, the absolute worst case of a
 * count-bounded cache. The budget **is** that figure directly now — there is
 * no count left to multiply by a per-entry cost.
 */
const ramMaxCache = computed(() => Number(settings.value.cover_cache_budget_mio) || 0)

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

async function loadAll() {
  // Needed here, not redundant: this is what reloads the catalog after a
  // successful language change (see `changeLanguage` below), in place of the
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
  // `current: null` = no saved choice: the "Default (system)" entry carries it
  // — no more fallback to the first device (it was `null`, the PCM that
  // discards the sound, at the top of `aplay -L`).
  device.value = audio.value.current ?? SYSTEM_DEFAULT
  lang.value = locale.value.current ?? 'en'
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
  }
  return [...byName.values()].map((acc) => {
    // "unknown" is never shown next to a real kind: we only keep it when it is
    // the only information received for this name. This holds by construction,
    // over the complete set of received kinds — not by looking only at what the
    // accumulator held at a given instant, which would depend on the arrival
    // order of the lines.
    const realKinds = acc.receivedKinds.filter((k) => k !== 'unknown')
    const kinds = (realKinds.length > 0 ? realKinds : acc.receivedKinds).join(', ')
    return {
      name: acc.name,
      kinds,
      connected: acc.connected,
      stalled: acc.stalled,
      starting: acc.starting,
      disabled: acc.disabled,
      busy: acc.busy,
      admin: acc.admin,
    }
  })
})

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

async function changeOutput() {
  const err = await api.put('/api/audio-output', {
    device: device.value === SYSTEM_DEFAULT ? null : device.value,
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

async function saveSettings() {
  const err = await api.put('/api/settings', {
    ...settings.value,
    volume_repeat_initial_ms: Number(settings.value.volume_repeat_initial_ms),
    volume_repeat_interval_ms: Number(settings.value.volume_repeat_interval_ms),
    overlay_ms: Number(settings.value.overlay_ms),
    tens_window_ms: Number(settings.value.tens_window_ms),
    seek_step_s: Number(settings.value.seek_step_s),
    // The four rendition settings are sent **even when the switch is
    // unchecked**, and that is deliberate: the UI greys them out without
    // emptying them, so re-checking the switch finds the values that had been
    // set. Omitting them would drop them back to the core defaults (the struct
    // is `serde(default)`), i.e. silently lose a setting visible on screen.
    cover_source_max_mio: Number(settings.value.cover_source_max_mio),
    cover_max_edge_px: Number(settings.value.cover_max_edge_px),
    cover_jpeg_quality: Number(settings.value.cover_jpeg_quality),
    cover_max_bytes_ko: Number(settings.value.cover_max_bytes_ko),
    cover_max_pixels_mpx: Number(settings.value.cover_max_pixels_mpx),
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

// Changing the language reloads the catalogs instead of reloading the whole
// page as the old UI did.
async function changeLanguage() {
  const err = await api.put('/api/locale', { locale: lang.value })
  if (err) {
    toast.error(err)
    return
  }
  await loadAll()
}

/**
 * The table of contents: one entry per card, in template order. It is data
 * (like REMOTE_ROWS for the remote control): the view walks it for the nav AND
 * for the scroll observation.
 */
const SECTIONS = [
  { id: 'plugins', key: 'plugins_title' },
  { id: 'audio', key: 'audio_output' },
  { id: 'language', key: 'language' },
  { id: 'startup', key: 'startup_title' },
  { id: 'clock', key: 'clock_title' },
  { id: 'volume-hold', key: 'volume_hold_title' },
  { id: 'overlays', key: 'overlays_title' },
  { id: 'seek', key: 'seek_card_title' },
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
onUnmounted(() => observer?.disconnect())

function goTo(id: string) {
  active.value = id
  document.getElementById(id)?.scrollIntoView({ behavior: 'smooth' })
}
</script>

<template>
  <div class="flex gap-8">
    <div class="min-w-0 flex-1 space-y-4">
      <section id="plugins" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('plugins_title') }}</CardTitle></CardHeader>
          <CardContent>
            <table class="w-full text-sm">
              <thead class="text-muted-foreground">
                <tr>
                  <th class="text-left font-normal">{{ t('col_plugin') }}</th>
                  <th class="text-left font-normal">{{ t('col_kind') }}</th>
                  <th class="text-left font-normal">{{ t('col_state') }}</th>
                  <th class="text-left font-normal">{{ t('col_admin') }}</th>
                  <th class="text-left font-normal">{{ t('col_enabled') }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="p in plugins" :key="p.name" data-plugin-row class="border-t border-border">
                  <td class="py-1" data-plugin-name>{{ p.name }}</td>
                  <td data-plugin-kind>{{ p.kinds }}</td>
                  <td data-plugin-state>
                    <Badge
                      :variant="
                        p.disabled
                          ? 'outline'
                          : p.busy
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
                      <!-- "Busy" comes **before** "connected": a busy plugin is
                           reachable, and that is precisely why "connected" says
                           nothing useful. "Starting" comes **before** "stalled":
                           both say the plugin has not spoken yet, and only the
                           elapsed time tells them apart. Showing "stalled"
                           during a normal startup wrongly accused a perfectly
                           healthy binary. -->
                      {{
                        p.disabled
                          ? t('disabled')
                          : p.busy
                            ? t('busy')
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
                         same row, and the notification says what happened. -->
                    <Switch
                      data-plugin-toggle
                      :model-value="!p.disabled"
                      :disabled="inProgress.has(p.name)"
                      :aria-label="t('toggle_plugin', { name: p.name })"
                      @click="togglePlugin(p)"
                    />
                  </td>
                </tr>
              </tbody>
            </table>
          </CardContent>
        </Card>
      </section>

      <section id="audio" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('audio_output') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <!-- The card title is not associated with the trigger: without an
                 aria-label, the selector has no accessible name at all. -->
            <Select v-model="device">
              <SelectTrigger class="min-w-64" :aria-label="t('audio_output')"><SelectValue /></SelectTrigger>
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
            <Button data-audio-change :disabled="audioUnavailable" @click="changeOutput">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="language" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('language') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <Select v-model="lang">
              <SelectTrigger class="min-w-32" :aria-label="t('language')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <!-- Name of the language and not its code: "français" is read,
                     "fr" is guessed. The code remains the value sent to the core. -->
                <SelectItem v-for="l in locale.locales" :key="l" :value="l">
                  {{ languageName(l) }}
                </SelectItem>
              </SelectContent>
            </Select>
            <Button data-lang-change @click="changeLanguage">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="startup" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('startup_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <Select v-model="settings.startup_power">
              <SelectTrigger class="min-w-32" data-startup-select :aria-label="t('startup_title')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="on">{{ t('startup_on') }}</SelectItem>
                <SelectItem value="standby">{{ t('startup_standby') }}</SelectItem>
                <SelectItem value="previous">{{ t('startup_previous') }}</SelectItem>
              </SelectContent>
            </Select>
            <Button data-startup-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Date and time. Two separate settings, at the owner's request: the
           order of a date and the 12/24 h format do not vary together from one
           country to another. No time zone setting — the display runs on the
           device, the page formats in the browser's time zone, and a third
           setting could only contradict one of the two. -->
      <section id="clock" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('clock_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
            <label class="grid gap-1 text-sm">
              {{ t('clock_date_label') }}
              <Select v-model="settings.date_format">
                <SelectTrigger class="min-w-36" data-date-format-select :aria-label="t('clock_date_label')"><SelectValue /></SelectTrigger>
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
                <SelectTrigger class="min-w-36" data-clock-hours-select :aria-label="t('clock_hours_label')"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="24">{{ t('clock_24h') }}</SelectItem>
                  <SelectItem value="12">{{ t('clock_12h') }}</SelectItem>
                </SelectContent>
              </Select>
            </label>
            <Button data-clock-change @click="saveSettings">{{ t('change') }}</Button>
            <p class="w-full text-sm text-muted-foreground">{{ t('clock_hint') }}</p>
          </CardContent>
        </Card>
      </section>

      <section id="volume-hold" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('volume_hold_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
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
            <Button data-hold-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="overlays" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('overlays_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
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
            <Button data-overlays-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="seek" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('seek_card_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
            <label class="grid gap-1 text-sm">
              {{ t('seek_step_label') }}
              <Input type="number" min="1" max="120" class="w-28" data-seek-step-s
                v-model="settings.seek_step_s" />
            </label>
            <Button data-seek-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Covers. A single card, two tiers that must not be confused, and the
           layout carries that distinction: the source cap comes **first** and
           is never greyed out, because it applies whatever the switch says —
           it is the only guard left when re-encoding is unchecked. The switch
           comes next, and greys out the four settings that only describe the
           thumbnail.

           Greyed out, not emptied: the values stay readable and go back in the
           PUT (see `saveSettings`), so re-checking the switch finds what had
           been set. -->
      <section id="covers" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('cover_card_title') }}</CardTitle></CardHeader>
          <CardContent class="space-y-4">
            <!-- Outside the greyed-out re-encoding box, like the source cap:
                 this bound applies whatever happens. -->
            <label class="grid gap-1 text-sm">
              {{ t('cover_cache_entries_label') }}
              <Input type="number" min="8" max="256" step="1" class="w-28" data-cover-cache-entries
                v-model="settings.cover_cache_budget_mio" />
              <span class="max-w-md text-xs text-muted-foreground">{{ t('cover_cache_entries_help') }}</span>
              <span class="max-w-md text-xs text-muted-foreground" data-cover-cache-ram>
                {{ t('cover_cache_entries_ram', { size: ramMaxCache, cap: capPerCover }) }}
              </span>
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('cover_source_max_label') }}
              <Input type="number" min="1" max="20" class="w-28" data-cover-source-max
                v-model="settings.cover_source_max_mio" />
              <span class="text-xs text-muted-foreground">{{ t('cover_source_max_help') }}</span>
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
                 it once rather than field by field. -->
            <div
              data-cover-rendition-group
              :aria-disabled="!settings.cover_rendition"
              :class="['flex flex-wrap items-start gap-4', settings.cover_rendition ? '' : 'opacity-50']"
            >
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
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_bytes_label') }}
                <Input type="number" min="32" max="8192" class="w-28" data-cover-max-bytes
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_bytes_ko" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_bytes_help') }}</span>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_pixels_label') }}
                <Input type="number" min="1" max="64" class="w-28" data-cover-max-pixels
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_pixels_mpx" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_pixels_help') }}</span>
              </label>
            </div>

            <Button data-cover-change @click="saveSettings">{{ t('change') }}</Button>
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
