<script setup lang="ts">
import { api, Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { formatDuration, INTERNAL, type Data, type Send, type T } from './data'

const props = defineProps<{
  data: Data
  t: T
  send: Send
  frozen: boolean
  /**
   * Is the core playing this source, according to its pushed stream.
   *
   * Consulted **in addition** to `data.playing`, and the two together decide
   * whether the stop must be requested. Each covers a weakness of the other:
   * the plugin's flag could stay false after a startup where mpv briefly goes
   * idle before loading, and this view is blind if `EventSource` is
   * unavailable. Neither can be a false positive for an *other* source, so
   * combining them cannot cut the radio.
   */
  isActiveSource: boolean
}>()

/**
 * Beyond this number of tracks, the list is paginated.
 *
 * A list built from a share can run into thousands of rows; rendering that
 * many nodes at once freezes the tab for several seconds on a Raspberry Pi's
 * browser. Pagination was preferred over scroll virtualization because it
 * stays exact without measuring row height — and a browser `Ctrl+F` finds
 * what is displayed, instead of finding nothing in a virtual window.
 */
const PAGINATION_THRESHOLD = 200
const PAGE_SIZE = 100

const name = ref('')
const destination = ref(INTERNAL)
const toLoad = ref('')
const page = ref(0)

const tracks = computed(() => props.data.playlist)
const paginated = computed(() => tracks.value.length > PAGINATION_THRESHOLD)
const pages = computed(() => Math.max(1, Math.ceil(tracks.value.length / PAGE_SIZE)))

// The page that holds the current track: landing on page 1 of a three
// thousand-title list while the player is at the 1,800th helps no one.
watch(
  () => props.data.index,
  (i) => {
    if (!paginated.value) return
    page.value = Math.min(pages.value - 1, Math.max(0, Math.floor(i / PAGE_SIZE)))
  },
  { immediate: true },
)

// A removal can clear the last page: without this recalculation, the pane
// would display an empty window instead of the end of the list.
watch(pages, (n) => {
  if (page.value > n - 1) page.value = n - 1
})

const offset = computed(() => (paginated.value ? page.value * PAGE_SIZE : 0))
const window = computed(() =>
  paginated.value
    ? tracks.value.slice(offset.value, offset.value + PAGE_SIZE)
    : tracks.value,
)

/**
 * Path of the track whose full path is shown, or `null`.
 *
 * The displayed name is the `#EXTINF` title, failing that the file name
 * without its extension: neither says which folder — nor which source — the
 * track comes from, and two namesakes taken from two albums are otherwise
 * indistinguishable. The tooltip answers that with a hover; this row answers
 * it with a tap, the only gesture a touchscreen has.
 *
 * Held as the **path** and not as the track's rank: a rank designates another
 * track as soon as the list moves, and the open row would then name a file
 * that is no longer the one above it. Keyed by the path it cannot say the
 * wrong thing, and it goes away on its own when its track leaves the list —
 * `remove`, `clear` and `load` have nothing to maintain by hand. `move` does
 * close it, but for the eye and not for correctness: see there.
 */
const openPath = ref<string | null>(null)

function togglePath(path: string): void {
  openPath.value = openPath.value === path ? null : path
}

/** Save destinations: internal storage, then the writable roots. */
const destinations = computed(() => [
  INTERNAL,
  ...props.data.roots.filter((r) => r.writable).map((r) => r.name),
])

const saved = computed(() => props.data.saved)

// The choice is tracked by its **rank** in the list rendered by the plugin,
// not by a key made of the name and location: those two do form the identity
// of a saved list, but no separator can join them unambiguously — a list
// name contains spaces, a root name contains dashes. The rank avoids
// inventing yet another grammar, and it is recalculated as soon as the
// plugin renders another list.
watch(
  saved,
  (list) => {
    if (Number(toLoad.value) >= list.length) toLoad.value = '0'
  },
  { immediate: true },
)

function move(from: number, to: number): void {
  if (to < 0 || to >= tracks.value.length) return
  // A reorder shuffles the ranks under the user's eyes. The open row could not
  // name the wrong file — keyed by its path, it follows its own track — but
  // one no longer knows, at a glance, which of the tracks that have just moved
  // it belongs to. Closing is the unambiguous answer. Covers the drag as well:
  // `drop` comes through here.
  openPath.value = null
  void props.send({ op: 'move', from, to })
}

/**
 * **Absolute** rank of the track currently being dragged, or `null`.
 *
 * Absolute and not relative to the page: that is the index the plugin
 * expects, and a paginated list makes them diverge from the second page on.
 */
const dragging = ref<number | null>(null)

/**
 * Drops the dragged track in the place of the one being hovered.
 *
 * Drag-and-drop only covers the **visible** rows: beyond two hundred tracks
 * the list is paginated, and one cannot drag onto a page that is not shown.
 * The up/down buttons, on the other hand, cross pages — so they stay there,
 * and not only for the keyboard.
 */
function drop(to: number): void {
  if (dragging.value === null || dragging.value === to) {
    dragging.value = null
    return
  }
  move(dragging.value, to)
  dragging.value = null
}

async function remove(i: number): Promise<void> {
  // Removing the track being listened to stops playback, just like clearing
  // the list: continuing to play a file that is no longer there would be the
  // worst possible response. The comparison is made on the **displayed**
  // index, the one behind the highlight the user sees; `playing`, on the
  // other hand, is read back afterwards so as not to depend on a stale page
  // state.
  const thisTrack = props.data.index === i
  const state = await props.send({ op: 'remove', index: i })
  if (!state) return
  if (thisTrack && (state.playing || props.isActiveSource)) await api.post('/api/command', { cmd: 'Stop' })
}

async function clear(): Promise<void> {
  // Clearing during playback left the music playing on a now-empty list: the
  // plugin cannot ask mpv anything — SDK notifications carry no action — so
  // it is the page that requests the stop from the core, through the same
  // channel as the remote. A user gesture, not an initiative from the
  // plugin.
  //
  // **Only if this source is indeed the one playing**: without this
  // condition, clearing an idle files list would cut the radio.
  // The state read is the one **rendered by the clearing**, not the one the
  // page displayed before. This is a measured fragility: `data` can be
  // stale — the page does not poll continuously — and a `playing` falsely
  // set to false silenced the stop request without anything signalling it.
  // Clearing does not touch `playing`, so reading it back still tells the
  // truth about what is playing.
  const state = await props.send({ op: 'clear' })
  if (!state) return
  if (state.playing || props.isActiveSource) await api.post('/api/command', { cmd: 'Stop' })
}

function save(): void {
  const n = name.value.trim()
  if (!n) return
  void props.send({ op: 'save_playlist', name: n, where: destination.value })
}

function load(): void {
  const choice = saved.value[Number(toLoad.value)]
  if (!choice) return
  void props.send({ op: 'load_playlist', name: choice.name, where: choice.where })
}
</script>

<template>
  <!-- No title here: the tab that opens this pane already carries the same
       word, and repeating it just below said nothing more. The pane does
       not lose its accessible name for that — `TabsContent` carries an
       `aria-labelledby` pointing at its trigger, that is, at that label. -->
  <section class="space-y-3" data-playlist-pane>
    <p v-if="!tracks.length" class="text-sm text-muted-foreground" data-empty-playlist>
      {{ t('empty_playlist') }}
    </p>

    <template v-else>
      <p v-if="paginated" class="flex items-center gap-2 text-sm text-muted-foreground">
        <Button
          variant="ghost"
          size="sm"
          data-page-prev
          :disabled="page === 0"
          @click="page -= 1"
        >
          ‹
        </Button>
        <span data-page-label>
          {{
            t('page_range', {
              from: offset + 1,
              to: offset + window.length,
              total: tracks.length,
            })
          }}
        </span>
        <Button
          variant="ghost"
          size="sm"
          data-page-next
          :disabled="page >= pages - 1"
          @click="page += 1"
        >
          ›
        </Button>
      </p>

      <table class="w-full text-sm">
        <thead class="text-muted-foreground">
          <tr>
            <th class="w-12 text-left font-normal">{{ t('col_num') }}</th>
            <th class="text-left font-normal">{{ t('col_track') }}</th>
            <th class="w-20 text-left font-normal">{{ t('col_duration') }}</th>
            <th class="w-28" />
          </tr>
        </thead>
        <tbody>
          <!-- Draggable rows, like the station grid of the radio plugin.
               `dragover.prevent` is essential: without it the browser
               refuses the drop. The rank sent to the plugin is **absolute**
               (`offset + i`) and not the page's own.

               A `<template>` and not a bare `<tr>`: each track may be
               followed by a second row carrying its full path, and the two
               belong to the same iteration. -->
          <template v-for="(p, i) in window" :key="`${offset + i}:${p.path}`">
            <tr
              data-track-row
              class="border-t border-border"
              :class="[
                offset + i === data.index ? 'bg-muted/50' : '',
                dragging === offset + i ? 'opacity-50' : '',
              ]"
              :draggable="!frozen"
              @dragstart="dragging = offset + i"
              @dragover.prevent
              @drop.prevent="drop(offset + i)"
              @dragend="dragging = null"
            >
              <!-- `data-track-num` carries the **number alone**, not the cell:
                   the drag handle also lives in it, and a test reading the
                   cell's text would find the glyph in it. -->
              <td class="whitespace-nowrap tabular-nums text-muted-foreground">
                <span
                  class="cursor-grab select-none pr-1"
                  :title="t('reorder_hint')"
                  data-drag-handle
                >
                  ⠿
                </span>
                <span data-track-num>{{ offset + i + 1 }}</span>
              </td>
              <td class="py-1 pr-2">
                <!-- A button and not plain text: the full path is reachable by
                     hovering (the `title`) as much as by tapping, and a
                     touchscreen only has the second gesture. Never disabled by
                     `frozen`, unlike the buttons on the right — reading a path
                     modifies nothing, and refusing it while an operation runs
                     would refuse it exactly when one wonders which file is
                     concerned. The dotted underline is what says, without any
                     hover, that there is something under the name. -->
                <button
                  type="button"
                  data-track-name
                  class="cursor-pointer text-left underline decoration-dotted underline-offset-4"
                  :title="p.path"
                  :aria-expanded="openPath === p.path"
                  @click="togglePath(p.path)"
                >
                  {{ p.name }}
                </button>
                <!-- A missing track is **flagged, never hidden**: a list that
                     shrinks on its own is a defect that takes months to
                     attribute, whereas an unmounted share is diagnosed in one
                     second when the tracks stay there, flagged. -->
                <span
                  v-if="p.missing === true"
                  data-track-missing
                  :title="p.path"
                  class="ml-2 rounded border border-destructive px-1 text-xs text-destructive"
                >
                  {{ t('missing_badge') }}
                </span>
                <!-- `null`: the mount was not answering, so it is not known. A
                     distinct, discreet badge, grey rather than red — saying
                     "missing" here would blame the file for a failure that is
                     the share's. The banner above gives the cause. -->
                <span
                  v-else-if="p.missing === null"
                  data-track-unknown
                  :title="p.path"
                  class="ml-2 rounded border border-muted-foreground px-1 text-xs text-muted-foreground"
                >
                  {{ t('missing_unknown') }}
                </span>
              </td>
              <td class="tabular-nums text-muted-foreground">{{ formatDuration(p.duration_s) }}</td>
              <td class="whitespace-nowrap">
                <Button
                  variant="ghost"
                  size="icon"
                  data-track-up
                  :aria-label="t('btn_move_up')"
                  :disabled="frozen || offset + i === 0"
                  @click="move(offset + i, offset + i - 1)"
                >
                  ▲
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  data-track-down
                  :aria-label="t('btn_move_down')"
                  :disabled="frozen || offset + i === tracks.length - 1"
                  @click="move(offset + i, offset + i + 1)"
                >
                  ▼
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  data-track-remove
                  :aria-label="t('btn_remove_track')"
                  :disabled="frozen"
                  @click="remove(offset + i)"
                >
                  ✕
                </Button>
              </td>
            </tr>
            <!-- The full path, revealed by the name above it. It carries no
                 `data-track-row`: that marker counts the tracks — pagination
                 asserts a hundred of them, then fifty — and a second row per
                 track would make it count something else. No `border-t`
                 either, so it reads as the continuation of its track and not
                 as a row of its own; it takes over the highlight of the track
                 being played for the same reason.

                 `break-all`: an absolute path on a share has no space to break
                 at, and would otherwise widen the table past the screen. -->
            <tr v-if="openPath === p.path" :class="offset + i === data.index ? 'bg-muted/50' : ''">
              <td
                colspan="4"
                data-track-path
                class="break-all pb-1 pl-8 pr-2 text-xs text-muted-foreground"
              >
                {{ p.path }}
              </td>
            </tr>
          </template>
        </tbody>
      </table>

      <Button variant="outline" data-clear :disabled="frozen" @click="clear">
        {{ t('btn_clear') }}
      </Button>
    </template>

    <!-- What loading an m3u could not resolve. Without this box, the loaded
         list would simply be shorter than the file, with nothing saying so. -->
    <div
      v-if="data.unresolved.length"
      data-unresolved
      class="space-y-1 rounded-md border border-border p-2"
    >
      <p class="text-sm font-medium">
        {{ t('unresolved_title', { count: data.unresolved.length }) }}
      </p>
      <ul class="text-xs text-muted-foreground">
        <li v-for="u in data.unresolved" :key="u" data-unresolved-row>{{ u }}</li>
      </ul>
    </div>

    <div class="flex flex-wrap items-end gap-2">
      <Input
        v-model="name"
        data-playlist-name
        class="w-48"
        :placeholder="t('ph_playlist_name')"
      />
      <select
        v-model="destination"
        data-playlist-where
        class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
        :aria-label="t('dest_label')"
      >
        <!-- Only the **writable** roots are offered: a share mounted
             read-only would refuse the write on the plugin side, and
             offering it here would only produce a refusal. -->
        <option v-for="d in destinations" :key="d" :value="d">
          {{ d === INTERNAL ? t('dest_internal') : d }}
        </option>
      </select>
      <Button data-save-playlist :disabled="frozen" @click="save">
        {{ t('btn_save_playlist') }}
      </Button>
    </div>

    <div class="flex flex-wrap items-end gap-2">
      <p v-if="!saved.length" class="text-sm text-muted-foreground" data-no-saved>
        {{ t('no_saved') }}
      </p>
      <template v-else>
        <select
          v-model="toLoad"
          data-saved-pick
          class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
          :aria-label="t('load_playlist_label')"
        >
          <!-- The location is displayed with the name: two "Jazz" lists can
               coexist, one internal, the other on the share. -->
          <option v-for="(s, i) in saved" :key="`${s.where}/${s.name}`" :value="String(i)">
            {{ s.name }} — {{ s.where === INTERNAL ? t('dest_internal') : s.where }}
          </option>
        </select>
        <Button variant="secondary" data-load-playlist :disabled="frozen" @click="load">
          {{ t('btn_load_playlist') }}
        </Button>
      </template>
    </div>
  </section>
</template>
