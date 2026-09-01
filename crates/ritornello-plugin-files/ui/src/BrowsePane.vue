<script setup lang="ts">
import { Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { truncateStart, type Data, type Entry, type Send, type T } from './data'

/**
 * The file browser of a declared source.
 *
 * A single level on screen, and not a tree one unfolds: on a real library, the
 * unfolded tree became taller than the page and the useful gesture — going down
 * — got lost among the rows of the previous levels. Same shape as the
 * declaration wizard (`FolderPicker`), except that here files are shown, and
 * not only folders.
 */
const props = defineProps<{ data: Data; t: T; send: Send; frozen: boolean }>()

/** Path of the top level of a root: the empty string, as on the plugin side. */
const TOP = ''

const root = ref('')
/** Open folder, relative to the root. */
const path = ref(TOP)
/**
 * Content of the open folder.
 *
 * Kept here rather than read directly from `data.browse`: the plugin stores
 * browse **and** search in the same place, so a search would empty the list
 * before the user's eyes. `null` as long as nothing has succeeded — which is
 * not the same thing as an empty folder.
 */
const entries = ref<Entry[] | null>(null)
const query = ref('')
const results = ref<Entry[] | null>(null)
const truncated = ref(false)
/** The search was interrupted before it had seen everything, distinct from `truncated`. */
const abort = ref(false)

function isOpen(name: string): boolean {
  return props.data.roots.some((r) => r.name === name)
}

/** Changing root or folder: what was displayed no longer speaks of the right one. */
function reset(): void {
  path.value = TOP
  entries.value = null
  results.value = null
  truncated.value = false
  abort.value = false
  query.value = ''
}

watch(
  // A root name can contain neither a space nor a comma (`champ_sur`, on the
  // plugin side): joining them with a space does give an injective fingerprint.
  () => props.data.roots.map((r) => r.name).join(' '),
  () => {
    // The chosen root may have disappeared from one save to the next: without
    // this realignment, the pane would keep addressing its `browse` to a name
    // the plugin no longer knows, and would only display refusals.
    if (isOpen(root.value)) return
    root.value = props.data.roots[0]?.name ?? ''
    reset()
    if (root.value) void load(TOP)
  },
  { immediate: true },
)

function changeRoot(name: string): void {
  if (name === root.value) return
  root.value = name
  reset()
  void load(TOP)
}

async function load(target: string): Promise<void> {
  if (!root.value) return
  const state = await props.send({ op: 'browse', root: root.value, path: target })
  // Refusal: nothing is stored. Storing an empty level would make it pass for
  // an empty folder, and the user would have no way to retry without reloading
  // the page.
  if (!state) return
  const nav = state.browse
  // Only the answer to the request we just made is accepted: browse and search
  // are stored in the same place on the plugin side, and a late answer would
  // fill the wrong level. An empty `query` is what tells a browse apart from a
  // search over the same folder.
  if (nav.root !== root.value || nav.path !== target || nav.query !== '') return
  // The displayed results belong to the folder where the search took place:
  // changing it means changing context. Without this clearing, `search_scope`
  // — a `computed` on the open folder — updates on its own, and the caption
  // announces the new folder above results that come from the old one.
  // Compared to the **accepted** path, and not triggered on every call: `load`
  // is also invoked on first display and by the root realignment, where there
  // is nothing to clear yet — and clearing unconditionally there would cancel
  // an unrelated input being typed.
  if (target !== path.value) {
    results.value = null
    truncated.value = false
    abort.value = false
    query.value = ''
  }
  path.value = target
  entries.value = nav.entries
}

function descend(name: string): void {
  void load(path.value ? `${path.value}/${name}` : name)
}

function goUp(): void {
  if (!path.value) return
  void load(path.value.replace(/\/?[^/]+$/, ''))
}

/**
 * Address of the open folder, root name included.
 *
 * The plugin's path is relative to the root: displayed alone, it does not say
 * which one we are in as soon as several sources are declared.
 */
const displayedPath = computed(() => [root.value, path.value].filter(Boolean).join('/'))
/** Truncated **from the start**: on a path, the useful information is the end. */
const shortPath = computed(() => truncateStart(displayedPath.value))

async function search(): Promise<void> {
  const q = query.value.trim()
  if (!q) {
    // The three go together: without them, a truncated or abandoned search
    // would leave these flags true behind a null `results` — inert today (the
    // whole block is hidden by `v-if="results"`), but this is the pair of
    // states the correction loop of task 6 worked to keep consistent
    // everywhere else.
    results.value = null
    truncated.value = false
    abort.value = false
    return
  }
  const target = path.value
  const state = await props.send({ op: 'search', root: root.value, path: target, query: q })
  if (!state) return
  const nav = state.browse
  if (nav.root !== root.value || nav.path !== target || nav.query !== q) return
  results.value = nav.results
  // The plugin caps the search: without this flag, a truncated list would pass
  // for complete and the user would conclude that their file is not there.
  truncated.value = nav.truncated
  // Distinct cause: a walk interrupted before it had seen everything is not the
  // same thing as a pattern that is too broad. Confusing them made the page
  // show "No result" for a search that had simply given up before reaching the
  // wanted file.
  abort.value = nav.abort
}

function addFolder(target: string): void {
  // Recursive and **asynchronous** on the plugin side: the answer does not wait
  // for the end of the scan, it is the page's probe that shows its progress.
  void props.send({ op: 'add_dir', root: root.value, path: target })
}

function addFile(target: string): void {
  void props.send({ op: 'add_file', root: root.value, path: target })
}

/**
 * Loads an m3u found while browsing: it **replaces** the current playlist.
 *
 * Distinct from the dropdown of *saved* playlists in the Playlist pane: that
 * one looks a name up in a store, whereas here a file is designated by its
 * path, where it sits on the source.
 */
function loadPlaylist(target: string): void {
  void props.send({ op: 'load_m3u', root: root.value, path: target })
}
</script>

<template>
  <!-- No title, like the two other panes: the tab already carries it, and
       `TabsContent` makes it the accessible name of this section. -->
  <section class="space-y-3" data-browse-pane>
    <p v-if="!data.roots.length" class="text-sm text-muted-foreground">
      {{ t('no_sources') }}
    </p>

    <template v-else>
      <div class="flex flex-wrap items-center gap-2">
        <label class="text-sm text-muted-foreground" for="root-parcourue">
          {{ t('root_label') }}
        </label>
        <select
          id="root-parcourue"
          data-browse-root
          class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
          :value="root"
          :disabled="frozen"
          @change="changeRoot(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="r in data.roots" :key="r.name" :value="r.name">{{ r.name }}</option>
        </select>
      </div>

      <!-- `min-w-0` wherever long text goes down: the minimum width of a flex
           child defaults to that of its content, and a long path would push
           the row out of the frame. It is also what makes `truncate` work. -->
      <div class="flex min-w-0 items-center gap-2 text-sm">
        <Button
          variant="ghost"
          size="sm"
          class="shrink-0"
          data-browse-up
          :disabled="frozen || !path"
          @click="goUp"
        >
          ↑ {{ t('btn_up') }}
        </Button>
        <span
          class="min-w-0 flex-1 truncate text-muted-foreground"
          data-browse-path
          :title="displayedPath"
        >
          {{ shortPath }}
        </span>
        <!-- Absent at the top: adding the whole source lives on the source's
             row, in the Sources pane. Two buttons for the same effect made one
             look for a difference that did not exist. -->
        <Button
          v-if="path"
          variant="secondary"
          size="sm"
          data-add-current
          :disabled="frozen"
          @click="addFolder(path)"
        >
          {{ t('btn_add_current_folder') }}
        </Button>
      </div>

      <!-- The search lives **above** the listing: it bears on the open folder,
           and the `data-search-scope` line names it right below, which is
           enough to state its scope. Under a list that had become as long as
           the folder, one had to scroll to find it. -->
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="query"
          data-search-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="search"
        />
        <Button data-search :disabled="frozen" @click="search">{{ t('btn_search') }}</Button>
      </div>
      <p class="text-xs text-muted-foreground" data-search-scope>
        {{ t('search_scope', { path: displayedPath }) }}
      </p>

      <div v-if="results" class="space-y-1" data-search-results>
        <!-- Reserved for the **complete** walk: a walk interrupted before it
             had seen everything says nothing about the presence of the file,
             and announcing it as "No result" would assert the opposite. -->
        <p
          v-if="!results.length && !abort"
          class="text-sm text-muted-foreground"
          data-no-results
        >
          {{ t('no_results') }}
        </p>
        <!-- The plugin's cap is silent in the list: without this sentence, a
             truncated search would pass for complete and the user would
             conclude that their file is not there. -->
        <p v-if="truncated" class="text-sm text-muted-foreground" data-search-truncated>
          {{ t('search_truncated', { count: results.length }) }}
        </p>
        <!-- Cause distinct from `truncated`: here the walk gave up before it
             had browsed everything, it did not find more than what it reports.
             So the advice is different: go down into a subfolder rather than
             refine the pattern. -->
        <p v-if="abort" class="text-sm text-muted-foreground" data-search-gave-up>
          {{ t('search_gave_up') }}
        </p>
        <!-- A search only reports files: `scan::search` filters on audio, and
             `normalizeBrowse` hard-codes `dir: false` for its results. The
             ternary that distinguished a folder here thus had a provably dead
             branch, and the key does not have to carry a type that does not
             vary. -->
        <div
          v-for="e in results"
          :key="e.path"
          class="flex min-w-0 items-center gap-2 text-sm"
          data-search-row
        >
          <!-- The full path, not only the name: a search reports namesakes from
               different folders, and nothing else allows telling them apart. -->
          <span class="min-w-0 flex-1 truncate">{{ e.path }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            :disabled="frozen"
            @click="addFile(e.path)"
          >
            {{ t('btn_add_to_playlist') }}
          </Button>
        </div>
      </div>

      <!-- No bounded height here: the list scrolls **with** the page. A frame
           with its own scrollbar nested two scrolls, and the wheel stopped at
           the edge of the list instead of continuing the page. Nothing is
           pushed off screen since the search is above. -->
      <ul class="min-w-0 space-y-1 text-sm" data-browse-list>
        <li
          v-for="e in entries ?? []"
          :key="`${e.dir ? 'd' : 'f'}:${e.path}`"
          data-browse-row
          class="flex min-w-0 items-center gap-2"
        >
          <template v-if="e.dir">
            <button
              type="button"
              data-browse-dir
              class="min-w-0 flex-1 truncate rounded px-2 py-1 text-left hover:bg-accent"
              :disabled="frozen"
              :title="e.name"
              @click="descend(e.name)"
            >
              <span aria-hidden="true" class="mr-1">📁</span
              ><span data-browse-name>{{ e.name }}</span>
            </button>
            <Button
              variant="secondary"
              size="sm"
              data-add-dir
              :disabled="frozen"
              @click="addFolder(e.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
          <!-- A playlist carries a **different** action: it replaces the
               current playlist instead of being added to it. Confusing them
               would add a text file that mpv would try to play. -->
          <template v-else-if="e.playlist">
            <span class="min-w-0 flex-1 truncate px-2">
              <span aria-hidden="true" class="mr-1">☰</span
              ><span data-browse-name>{{ e.name }}</span>
            </span>
            <Button
              variant="secondary"
              size="sm"
              data-load-m3u
              :disabled="frozen"
              @click="loadPlaylist(e.path)"
            >
              {{ t('btn_load_m3u') }}
            </Button>
          </template>
          <template v-else>
            <span class="min-w-0 flex-1 truncate px-2" data-browse-name>{{ e.name }}</span>
            <Button
              variant="ghost"
              size="sm"
              data-add-file
              :disabled="frozen"
              @click="addFile(e.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
        </li>
        <!-- `entries` non-null, so a level was indeed reported: a genuinely
             empty folder, and not a browse that did not succeed. -->
        <li
          v-if="entries && !entries.length"
          class="px-2 text-muted-foreground"
          data-browse-empty
        >
          {{ t('empty_folder') }}
        </li>
      </ul>
    </template>
  </section>
</template>
