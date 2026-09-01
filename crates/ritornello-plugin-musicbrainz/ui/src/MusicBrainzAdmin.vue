<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, createT, Input, Label, toast, type Catalog } from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` is part of the plugin UI contract, just like `catalog`: the
// **absolute** prefix under which the core serves this plugin's routes
// (`/plugins/musicbrainz/`), provided by the shell. **Required** prop, no
// default value, for the same reason as in `MpdAdmin.vue`: the name under
// which this plugin is served comes from `plugins.toml`, hence from the
// deployment, and a default would be wrong — silently — as soon as an
// operator declares it under another name.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** Absolute URL of one of this plugin's routes, built from `base`. */
function url(path: string): string {
  return `${props.base}${path}`
}

// --- The get_data / set_data contract (task 8), copied here as is ----------
//
// `pattern` is an **externally tagged** enumeration: either the object
// `{ split: {...} }`, or the bare string `"do_not_split"`. It is not an
// object with a `type` field — typing it as a union of these two exact shapes
// avoids reconstructing a shape that does not exist on the server side.
interface SplitPattern {
  split: {
    separator: string
    artist_first: boolean
    /** The `Artist - Title - Album` shape: the title is the middle field.
     *
     * Optional because the field is additive on the backend side
     * (`serde(default)`), so a state file written before it reads back without
     * it. And the page never **produces** it: this pattern is only obtained by
     * probing, never by hand — the closed set of the editor does not offer it. */
    title_in_middle?: boolean
  }
}
type Pattern = SplitPattern | 'do_not_split'
type Origin = 'standard_confirmed' | 'learned_deviation' | 'manual'

interface Station {
  url: string
  pattern: Pattern
  origin: Origin
  // Present and null when the station has never served (not absent): the type
  // carries this possibility explicitly rather than handling it downstream as
  // an optional field that could also be missing.
  last_used: string | null
  split_titles: number
  /** Probes that concluded "do not split" on real evidence.
   *
   * What makes such a verdict **provisional**: a single title proves nothing,
   * so as long as this stays under the threshold a new splittable string
   * reopens the question. Optional because the field is additive on the
   * backend side (`serde(default)`), so a state file written before it reads
   * back without it. */
  failed_probes?: number
}

interface Data {
  stations: Station[]
  /** Threshold beyond which a "do not split" is anchored.
   *
   * **Sent by the server**, never held here: it is a decision of the probing
   * logic, and a copy in this file would be free to drift from the one that
   * actually decides. Optional for the same reason as `failed_probes`. */
  probes_before_anchoring?: number
}

const data = ref<Data>({ stations: [] })

/**
 * "Exceptions only" filter, **active by default**: a station whose format has
 * been confirmed standard does exist as an entry (its absence would confuse
 * "never probed" with "verified compliant"), but what the operator comes here
 * for are the stations that deviate — this filter isolates them from the
 * noise of stations that already work.
 */
const filterExceptions = ref(true)

const shownStations = computed(() =>
  filterExceptions.value
    ? data.value.stations.filter((s) => s.origin !== 'standard_confirmed')
    : data.value.stations,
)

// Two distinct empty states, never merged: an empty screen would otherwise be
// ambiguous between "all is well" and "nothing has ever worked".
const nothingProbed = computed(() => data.value.stations.length === 0)
const filterHidesAll = computed(() => !nothingProbed.value && shownStations.value.length === 0)

async function reload(): Promise<void> {
  try {
    data.value = await api.get<Data>(url('api/data'))
  } catch (e) {
    // Like `MpdAdmin.vue`: no catalog key covers this load failure, the raw
    // request message is the only text available.
    toast.error((e as Error).message)
  }
}

onMounted(reload)

// --- Labels ---------------------------------------------------------------

// Literal calls (`t.value('origin_standard')`, etc.), not an indirection
// through a table: `i18nKeysUsed.test.ts` only collects keys passed in plain
// text to `t`/`t.value`, a key recomposed from a variable would silently
// escape it.
function originText(o: Origin): string {
  switch (o) {
    case 'standard_confirmed':
      return t.value('origin_standard')
    case 'learned_deviation':
      return t.value('origin_learned')
    case 'manual':
      return t.value('origin_manual')
  }
}

/**
 * State of a "do not split" verdict: still provisional, or anchored — and
 * `null` when the question does not arise.
 *
 * Shown **next to** the pattern rather than in a column of its own: it
 * concerns one pattern out of three, and a column empty on most rows would
 * cost more width than it gives information, on a page whose stream URLs
 * already scroll.
 *
 * `null` for a manual pattern even when it does not split: nothing reprobes an
 * operator's decision, so calling it "provisional" would be false.
 */
function unsplitState(st: Station): string | null {
  if (st.pattern !== 'do_not_split' || st.origin === 'manual') return null
  const total = data.value.probes_before_anchoring
  const done = st.failed_probes ?? 0
  // Threshold absent (an older server, or a field not yet sent): say nothing
  // rather than guess a number. A wrong "2 of 5" would be worse than silence.
  if (total === undefined) return null
  return done >= total
    ? t.value('pattern_no_split_anchored')
    : t.value('pattern_no_split_provisional', { done, total })
}

function patternText(m: Pattern): string {
  if (m === 'do_not_split') return t.value('pattern_no_split')
  // The `Artist - Title - Album` shape carries the same separator and the
  // same order as the standard: without its own mention, it would display
  // like it and the page would lie by omission on the only column one comes
  // here to read.
  const order = m.split.title_in_middle
    ? t.value('pattern_title_middle')
    : m.split.artist_first
      ? t.value('pattern_artist_first')
      : t.value('pattern_title_first')
  return `"${m.split.separator}" (${order})`
}

// --- Editing --------------------------------------------------------------
//
// A closed set, never a regular expression: a free regex would have the user
// debugging expressions, and a bad one would break every title of the
// station. The only choices are a separator (a string, not a pattern), an
// order (two values), and "do not split", which greys out the previous two.

/** URL of the station being edited, `null` if none. One row at a time:
 *  opening a second edit implicitly closes the first (see `openEdit`). */
const rowBeingEdited = ref<string | null>(null)
const edSeparator = ref('')
const edOrder = ref<'artist_first' | 'title_first'>('artist_first')
const edDoNotSplit = ref(false)
/** The `Artist - Title - Album` shape, **preserved but not offered**: no form
 * field sets it, but editing an entry that carries it must replay it
 * identically. See `openEdit`. */
const edTitleInMiddle = ref(false)

function openEdit(s: Station): void {
  rowBeingEdited.value = s.url
  if (s.pattern === 'do_not_split') {
    edDoNotSplit.value = true
    edSeparator.value = ''
    edOrder.value = 'artist_first'
  } else {
    edDoNotSplit.value = false
    edSeparator.value = s.pattern.split.separator
    edOrder.value = s.pattern.split.artist_first ? 'artist_first' : 'title_first'
    // Preserved, not offered: the form does not propose this shape — it is
    // only obtained by probing — but it must **replay** it as is. Without this
    // line, opening the edit of a station in "Artist - Title - Album" then
    // saving without changing anything degraded its pattern, and as the entry
    // became manual, nothing repaired it anymore.
    edTitleInMiddle.value = s.pattern.split.title_in_middle === true
  }
}

function cancelEdit(): void {
  rowBeingEdited.value = null
}

/**
 * Validation error of the separator, or `null` if it is valid — recomputed on
 * every keystroke. Reuses **the same catalog keys** as those the backend
 * returns for these two precise refusals (`separator_empty`,
 * `separator_no_space`), so that the immediate page-side feedback says
 * exactly what a server refusal would say. It does not apply when "do not
 * split" is checked: the separator is then out of play.
 */
const separatorError = computed(() => {
  if (edDoNotSplit.value) return null
  // `trim()` and not mere emptiness: a separator that is only spaces passed
  // both checks (`' '` starts *and* ends with a space, the same one) and would
  // have split on every space of the announced title. Same predicate as the
  // backend, which remains the authority.
  if (!edSeparator.value.trim()) return t.value('separator_empty')
  if (!(edSeparator.value.startsWith(' ') && edSeparator.value.endsWith(' '))) {
    return t.value('separator_no_space')
  }
  return null
})

function buildPattern(): Pattern {
  if (edDoNotSplit.value) return 'do_not_split'
  return {
    split: {
      separator: edSeparator.value,
      artist_first: edOrder.value === 'artist_first',
      title_in_middle: edTitleInMiddle.value,
    },
  }
}

/**
 * Posts the `set` action. The page validates the separator for immediate
 * feedback (`separatorError`), **but** the backend remains the authority: this
 * same input may still be refused there (state file not writable, race with
 * another admin client), in which case its message — already a translated
 * sentence, never a key — is displayed as is, without retranslation.
 */
async function saveEdit(): Promise<void> {
  if (separatorError.value) return
  const stationUrl = rowBeingEdited.value
  if (!stationUrl) return
  const err = await api.put(url('api/data'), { action: 'set', url: stationUrl, pattern: buildPattern() })
  if (err) {
    toast.error(err)
    return
  }
  rowBeingEdited.value = null
  await reload()
}

async function remove(s: Station): Promise<void> {
  const err = await api.put(url('api/data'), { action: 'remove', url: s.url })
  if (err) {
    toast.error(err)
    return
  }
  // The removed row could be the one being edited: without this guard, the
  // form would stay open on a station that no longer exists.
  if (rowBeingEdited.value === s.url) rowBeingEdited.value = null
  await reload()
}

async function clear(): Promise<void> {
  const err = await api.put(url('api/data'), { action: 'clear' })
  if (err) {
    toast.error(err)
    return
  }
  rowBeingEdited.value = null
  await reload()
}
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t('title') }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p class="text-sm text-muted-foreground">{{ t('intro') }}</p>

      <div class="flex flex-wrap items-center justify-between gap-2">
        <label class="flex items-center gap-2 text-sm">
          <input type="checkbox" data-filter-exceptions v-model="filterExceptions" />
          {{ t('filter_exceptions_only') }}
        </label>
        <Button variant="secondary" data-clear @click="clear">{{ t('clear_all') }}</Button>
      </div>

      <p v-if="nothingProbed" data-empty class="text-sm text-muted-foreground">{{ t('empty') }}</p>
      <p v-else-if="filterHidesAll" data-empty-filtered class="text-sm text-muted-foreground">
        {{ t('empty_filtered') }}
      </p>

      <!-- Scroll container specific to the table: a stream URL is long, and
           this page must not scroll the whole page to accommodate it. -->
      <div v-if="!nothingProbed && !filterHidesAll" class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead class="text-muted-foreground">
            <tr>
              <th class="text-left font-normal">{{ t('col_station') }}</th>
              <th class="text-left font-normal">{{ t('col_pattern') }}</th>
              <th class="text-left font-normal">{{ t('col_origin') }}</th>
              <th class="text-left font-normal">{{ t('col_last_used') }}</th>
              <th class="text-left font-normal">{{ t('col_split_count') }}</th>
              <th class="text-left font-normal">{{ t('col_actions') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in shownStations" :key="s.url" data-station-row class="border-t border-border align-top">
              <!-- `max-w-0` forces the column to respect the width of the
                   `<table>` rather than stretching to the length of the URL:
                   that is what lets `truncate` apply. -->
              <td class="max-w-0 truncate py-2 pr-2" :title="s.url">{{ s.url }}</td>

              <td class="py-2 pr-2">
                <template v-if="rowBeingEdited === s.url">
                  <div class="flex flex-col gap-1">
                    <Label class="text-xs font-normal text-muted-foreground">{{ t('field_separator') }}</Label>
                    <Input
                      data-separator v-model="edSeparator" :disabled="edDoNotSplit"
                      :aria-invalid="!!separatorError"
                    />
                    <Label class="text-xs font-normal text-muted-foreground">{{ t('field_order') }}</Label>
                    <select
                      data-order v-model="edOrder" :disabled="edDoNotSplit"
                      class="rounded-md border border-input bg-transparent px-2 py-1 text-sm disabled:opacity-50"
                    >
                      <option value="artist_first">{{ t('pattern_artist_first') }}</option>
                      <option value="title_first">{{ t('pattern_title_first') }}</option>
                    </select>
                    <label class="flex items-center gap-2">
                      <input type="checkbox" data-do-not-split v-model="edDoNotSplit" />
                      {{ t('field_no_split') }}
                    </label>
                    <p v-if="separatorError" data-separator-error class="text-xs text-destructive">
                      {{ separatorError }}
                    </p>
                  </div>
                </template>
                <template v-else>
                  {{ patternText(s.pattern) }}
                  <span
                    v-if="unsplitState(s)"
                    data-unsplit-state
                    class="text-xs text-muted-foreground"
                  >{{ unsplitState(s) }}</span>
                </template>
              </td>

              <td class="py-2 pr-2">{{ originText(s.origin) }}</td>
              <td class="py-2 pr-2">{{ s.last_used ?? '—' }}</td>
              <td class="py-2 pr-2">{{ s.split_titles }}</td>

              <td class="py-2">
                <template v-if="rowBeingEdited === s.url">
                  <div class="flex flex-wrap gap-1">
                    <Button size="sm" data-save-edit :disabled="!!separatorError" @click="saveEdit">
                      {{ t('save') }}
                    </Button>
                    <Button size="sm" variant="secondary" data-cancel-edit @click="cancelEdit">
                      {{ t('cancel') }}
                    </Button>
                  </div>
                </template>
                <template v-else>
                  <div class="flex flex-wrap gap-1">
                    <Button size="sm" variant="secondary" data-edit @click="openEdit(s)">
                      {{ t('edit') }}
                    </Button>
                    <Button size="sm" variant="secondary" data-remove @click="remove(s)">
                      {{ t('delete') }}
                    </Button>
                  </div>
                </template>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </CardContent>
  </Card>
</template>
