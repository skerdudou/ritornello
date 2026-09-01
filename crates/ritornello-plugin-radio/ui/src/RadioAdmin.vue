<script setup lang="ts">
import {
  api, Button, createT, Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger,
  Input, type Catalog,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'
import { move } from './order'
import CountryPicker from './CountryPicker.vue'
import { countryName, ALL_COUNTRIES, type Country } from './country'

// `base` is part of the plugin UI contract, same as `catalog`: the
// **absolute** prefix under which the core serves this plugin's routes
// (`/plugins/radio/`), provided by the shell.
//
// This view used to call `api.get('./api/data')` relatively — so resolved
// against the browser's URL, not against anything the contract guarantees.
// On `/plugins/radio` (without a trailing slash, a form the shell's router
// also accepted), `./api/data` resolved to `/plugins/api/data`, which the
// core interprets as the "api" plugin: 404, empty table, load error and
// every button failing.
//
// **Required** prop, with no default value: the name under which this
// plugin is served comes from `plugins.toml`, i.e. from deployment, not
// from this file. A default of `/plugins/radio/` would be wrong as soon as
// the operator declares this plugin under another name, and would be wrong
// *silently*. Better for the shell to be required to provide the prefix —
// which a `PluginView` test checks.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** Absolute URL of a route of this plugin, built from `base`. */
function url(path: string): string {
  return `${props.base}${path}`
}

// Same bound as the server-side validation (1..=99): the UI refuses the
// addition rather than letting the save fail. `Stations::validate` remains
// the server authority.
const MAX = 99

interface Station { name: string; url: string }
/**
 * Row being edited. The key is **browser-side only** and serves the loop's
 * `:key`: without a stable identity, reordering rows would reuse the input
 * fields in the wrong place (Vue matches by index) and focus would jump
 * during a drag-and-drop.
 */
interface Row extends Station { key: number }
interface FoundStation { name: string; url: string; codec: string; bitrate: number; country: string }

let nextKey = 0
function row(s: Station): Row {
  nextKey += 1
  return { key: nextKey, name: s.name, url: s.url }
}

const stations = ref<Row[]>([])
const results = ref<FoundStation[] | null>(null)
const query = ref('')
const country = ref(ALL_COUNTRIES)
const countryList = ref<Country[]>([])
const countriesOpen = ref(false)
const message = ref('')
const searching = ref(false)
// Safeguard carried over from the old page, which used to end its
// load-failure handler with `document.querySelectorAll('button').forEach((b)
// => { b.disabled = true })`. Its reason for being: after a failed GET,
// `stations` stays **empty** while the table served by the plugin, itself,
// is not. A "Save" would send `{op:'save', stations: []}`, which
// `Stations::validate` accepts (it iterates over an empty vector) and which
// overwrites `stations.toml`: all of the user's presets disappear, with no
// confirmation and no way back.
//
// This is not theoretical: the plugin serves admin requests strictly in
// series, with a 4 s directory budget against the core's 5 s cap, so a
// concurrent load during a search can make the GET fail while a later PUT
// succeeds. A plugin restart between the two produces the same effect.
//
// The state is **sticky**: like the old page, there is no "retry" here,
// only a page reload recovers a healthy state. Better an inert page than
// one that destroys data it failed to read.
const loadFailed = ref(false)

/**
 * Country button label, rendered from **our own** state.
 *
 * This is the fix for an observed bug: the previous version entrusted this
 * label to `<SelectValue>`, which captures the selected element's text on
 * first render. But `PluginView` mounts the UI with an **empty** catalog
 * (it is loaded asynchronously), so the captured text was the translation
 * key itself — the page literally displayed "country_fr" until the list
 * was opened.
 */
const countryLabel = computed(() =>
  country.value === ALL_COUNTRIES ? t.value('country_all') : countryName(country.value),
)

async function reload(): Promise<void> {
  try {
    const data = await api.get<{
      stations: Array<Station & { preset: number }>
      search?: FoundStation[]
      countries?: Country[]
      country?: string
    }>(url('api/data'))
    stations.value = [...data.stations]
      .sort((a, b) => a.preset - b.preset)
      .map((s) => row({ name: s.name, url: s.url }))
    if (data.search?.length) results.value = data.search
    if (data.countries?.length) countryList.value = data.countries
    // Country retained by the plugin: `??` and not `||`, an empty string
    // being a legitimate choice ("all countries") and not an absence of
    // value.
    country.value = data.country ?? ALL_COUNTRIES
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
    loadFailed.value = true
  }
}

onMounted(reload)

/**
 * Fetches the country list, once and **only when the picker opens**: it is
 * a network call nothing justifies as long as the user isn't trying to
 * change country.
 */
async function openCountries(open: boolean): Promise<void> {
  countriesOpen.value = open
  if (!open || loadFailed.value) return
  if (countryList.value.length || searching.value) return
  searching.value = true
  message.value = t.value('country_loading')
  try {
    const err = await api.put(url('api/data'), { op: 'countries' })
    if (err) {
      message.value = err
      return
    }
    const data = await api.get<{ countries?: Country[] }>(url('api/data'))
    countryList.value = data.countries ?? []
    message.value = ''
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
  } finally {
    searching.value = false
  }
}

function chooseCountry(code: string): void {
  country.value = code
  countriesOpen.value = false
}

// Nothing is persisted before "Save": the addition only acts on the table
// being edited.
function add(s: Station = { name: '', url: '' }): boolean {
  if (stations.value.length >= MAX) {
    message.value = t.value('limit_reached')
    return false
  }
  stations.value.push(row(s))
  message.value = ''
  return true
}

function remove(i: number): void {
  stations.value.splice(i, 1)
}

// Reordering: the preset **is** the position, so moving a row changes its
// remote number. Nothing is persisted before "Save", same as for adding
// and removing.
const dragging = ref<number | null>(null)

function drop(to: number): void {
  if (dragging.value === null) return
  stations.value = move(stations.value, dragging.value, to)
  dragging.value = null
}

/** Up/down buttons: drag-and-drop is neither keyboard-accessible nor reliable with a finger. */
function shift(i: number, step: number): void {
  stations.value = move(stations.value, i, i + step)
}

// Automatic numbering: the preset is the row's **position**. Accepted
// consequence: removing a row renumbers the following ones.
async function save(): Promise<void> {
  // Belt and braces: the protection does not rest on the button's visual
  // state alone. A `disabled` can be bypassed (dev tools, an extension, a
  // future template refactor that forgets the binding) while the
  // consequence — overwriting `stations.toml` with an empty table — is
  // irreversible.
  if (loadFailed.value) return
  const payload = stations.value.map((s, i) => ({ preset: i + 1, name: s.name, url: s.url }))
  const err = await api.put(url('api/data'), { op: 'save', stations: payload })
  message.value = err ? t.value('save_error') + err : t.value('saved')
}

// Single flight: the SDK serves admin requests strictly in series. A
// second trigger while a search is running would queue behind the first
// and, the directory being slow (4 s budget on the plugin side), would
// exceed the core's 5 s cap — which would answer with an error message
// (`plugin_timeout`) inappropriate for a legitimate action. The guard is
// shared by the button and the Enter key, and lifted in a `finally` so it
// recovers as well after an error as after a success.
async function search(): Promise<void> {
  // Same guard as `save()` (see its comment): `:disabled` on the button
  // does not protect `@keydown.enter`, which reaches `search()` even after
  // a failed load. Without this early return, a successful search would
  // do `message.value = ''`, erasing the load error message while
  // `loadFailed` stays true: the page would look healthy while it is
  // inert (see also the belt-and-braces guard at the start of `save()`).
  if (loadFailed.value) return
  if (searching.value) return
  const q = query.value.trim()
  if (!q) {
    message.value = t.value('empty_query')
    return
  }
  searching.value = true
  message.value = t.value('searching')
  try {
    const err = await api.put(url('api/data'), { op: 'search', query: q, country: country.value })
    if (err) {
      message.value = err
      return
    }
    const data = await api.get<{ search?: FoundStation[] }>(url('api/data'))
    results.value = data.search ?? []
    message.value = ''
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
  } finally {
    searching.value = false
  }
}

function label(s: FoundStation): string {
  return `${s.name} — ${s.codec} ${s.bitrate} kbps${s.country ? ` (${s.country})` : ''}`
}
</script>

<template>
  <div class="space-y-6">
    <table class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="w-16 text-left font-normal">{{ t('col_num') }}</th>
          <th class="text-left font-normal">{{ t('col_name') }}</th>
          <th class="text-left font-normal">{{ t('col_url') }}</th>
          <th class="w-24" />
        </tr>
      </thead>
      <tbody>
        <!--
          Draggable rows: since the preset is the position, dragging a
          station changes its number. `dragover.prevent` is essential —
          without it the browser refuses the drop.
        -->
        <tr
          v-for="(s, i) in stations"
          :key="s.key"
          class="border-t border-border"
          :class="dragging === i ? 'opacity-50' : ''"
          draggable="true"
          data-station-row
          @dragstart="dragging = i"
          @dragover.prevent
          @drop.prevent="drop(i)"
          @dragend="dragging = null"
        >
          <td class="tabular-nums text-muted-foreground">
            <span class="cursor-grab select-none pr-1" :title="t('reorder_hint')" data-drag-handle>⠿</span>
            <span data-station-num>{{ i + 1 }}</span>
          </td>
          <td class="py-1 pr-2"><Input v-model="s.name" data-station-name /></td>
          <td class="py-1 pr-2"><Input v-model="s.url" data-station-url /></td>
          <td class="whitespace-nowrap">
            <!-- Alternative to drag-and-drop: neither the keyboard nor a
                 touchscreen fares well with it. -->
            <Button
              variant="ghost"
              size="icon"
              data-station-up
              :aria-label="t('move_up')"
              :disabled="i === 0"
              @click="shift(i, -1)"
            >
              ▲
            </Button>
            <Button
              variant="ghost"
              size="icon"
              data-station-down
              :aria-label="t('move_down')"
              :disabled="i === stations.length - 1"
              @click="shift(i, 1)"
            >
              ▼
            </Button>
            <!-- With no accessible name, a screen reader announces the "✕"
                 glyph — its up/down neighbors already had one. -->
            <Button
              variant="ghost"
              size="icon"
              data-station-delete
              :aria-label="t('remove_station')"
              @click="remove(i)"
            >
              ✕
            </Button>
          </td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <!-- The three actions are neutralized when loading has failed,
           mirroring the old page's global disabling. -->
      <Button variant="secondary" data-add :disabled="loadFailed" @click="add()">
        {{ t('btn_add') }}
      </Button>
      <Button data-save :disabled="loadFailed" @click="save">{{ t('btn_save') }}</Button>
      <span class="text-sm text-muted-foreground">{{ message }}</span>
    </div>

    <section class="space-y-2">
      <h2 class="font-medium">{{ t('search_title') }}</h2>
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="query"
          data-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="search"
        />
        <Dialog :open="countriesOpen" @update:open="openCountries">
          <DialogTrigger as-child>
            <Button variant="outline" class="w-44 justify-start" data-country-open>
              {{ countryLabel }}
            </Button>
          </DialogTrigger>
          <DialogContent class="sm:max-w-md">
            <DialogHeader><DialogTitle>{{ t('country_label') }}</DialogTitle></DialogHeader>
            <CountryPicker
              :list="countryList"
              :current="country"
              :all-label="t('country_all')"
              :placeholder="t('country_filter_placeholder')"
              :empty-label="t('country_none')"
              @choose="chooseCountry"
            />
          </DialogContent>
        </Dialog>
        <Button data-search :disabled="searching || loadFailed" @click="search">
          {{ t('btn_search') }}
        </Button>
      </div>
      <ul v-if="results" class="space-y-1 text-sm">
        <li v-if="!results.length" class="text-muted-foreground">{{ t('no_results') }}</li>
        <li v-for="(s, i) in results" :key="i" class="flex items-center gap-2">
          <!-- textContent via interpolation, never v-html: the name comes
               from a public directory. -->
          <span class="flex-1">{{ label(s) }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            @click="add({ name: s.name, url: s.url })"
          >
            {{ t('btn_add_result') }}
          </Button>
        </li>
      </ul>
    </section>
  </div>
</template>
