<script setup lang="ts">
import {
  api,
  createT,
  onPlayer,
  Skeleton,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  useSkeleton,
  type Catalog,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { normalizeData, type Data } from './data'
import PlaylistPane from './PlaylistPane.vue'
import BrowsePane from './BrowsePane.vue'
import SourcesPane from './SourcesPane.vue'

// `base` is part of the contract of plugin UIs, just like `catalog`: the
// **absolute** prefix under which the core serves this plugin's routes
// (`/plugins/files/`), provided by the shell.
//
// **Required** prop, **without default value**: the name under which this
// plugin is served comes from `plugins.toml`, hence from the deployment. A
// default `/plugins/files/` would be wrong — silently — as soon as the operator
// declares this plugin under another name. And every URL is built from it: a
// relative `./api/data` would resolve against the browser URL, hence to
// `/plugins/api/data` on `/plugins/files` (without trailing slash), which the
// core interprets as a plugin named "api" — 404 and inert page.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

function url(path: string): string {
  return `${props.base}${path}`
}

/**
 * Probe period during a recursive scan.
 *
 * The admin protocol pushes **nothing**: there is neither an event channel nor
 * a websocket behind the admin socket, only request-responses. The only way to
 * watch an `add_dir` progress — which is asynchronous on the plugin side — is
 * therefore to ask `api/data` again.
 */
const PROBE_PERIOD_MS = 1000

const data = ref<Data | null>(null)
const message = ref('')
// True as long as the **first** load has not succeeded. This is the guard
// taken over from the radio page, and here it protects against damage of the
// same order: after a failed GET, `roots` is empty while `media-roots.toml` is
// not, and a "Save roots" would send `{op:'save_roots', roots: []}` — which
// overwrites the file and makes the declared shares disappear, without
// confirmation or way back.
//
// The failure of a **later** probe does not raise it: the data is then already
// there, it does not lie, and making the page inert because a one-second
// refresh failed would be a comfort regression for no safety gain.
const loadFailed = ref(false)

/**
 * Whether the first answer has come back — successful or not.
 *
 * Not `data !== null`, which is what the template used to key off: on a
 * refusal `data` stays null for good, so a placeholder tied to it would pulse
 * for ever, on top of the very message explaining why the page is inert.
 */
const loaded = ref(false)

// Nothing for the first fraction of a second, a placeholder only if the wait
// outlasts it. Same rhythm as the shell and the other plugins, held in the kit
// so it cannot drift.
const skeleton = useSkeleton(() => !loaded.value)

let timer: ReturnType<typeof setTimeout> | null = null

function stopProbe(): void {
  if (timer !== null) {
    clearTimeout(timer)
    timer = null
  }
}

/**
 * Is there work in progress whose end the page is waiting for?
 *
 * Two, and it took an end-to-end journey to remember it: the recursive scan,
 * **and** the connection to a share. The admin protocol pushes nothing, so
 * everything asynchronous on the plugin side only reaches the screen through
 * this probe. Watching only the scan left the network dialog stuck on
 * "Connecting…" forever — the plugin had answered, but nobody was reading it
 * back anymore.
 */
function workInProgress(): boolean {
  return (
    data.value?.scan.running === true ||
    data.value?.explore.busy === true ||
    // The duration survey: they arrive in batches, and without this probe the
    // column would stay at "—" until the user's next gesture.
    data.value?.durations.running === true
  )
}

/**
 * A wizard is open: it carries the refusals, not the page.
 *
 * The page banner lives **behind** the grey veil of the dialog. Leaving it
 * there in duplicate amounted to showing the refusal where it cannot be read,
 * at the very moment it matters.
 */
const popoverOpen = computed(() => data.value?.explore.open === true)

function scheduleProbe(): void {
  stopProbe()
  if (!workInProgress()) return
  timer = setTimeout(() => {
    void reload()
  }, PROBE_PERIOD_MS)
}

async function reload(): Promise<void> {
  try {
    data.value = normalizeData(await api.get<unknown>(url('api/data')))
    scheduleProbe()
  } catch (e) {
    // The load message does not overwrite a refusal already displayed if there
    // is one: both tell the same incident, and the first is the more precise
    // (it comes from the server's catalog).
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
    if (data.value === null) loadFailed.value = true
    stopProbe()
  } finally {
    loaded.value = true
  }
}

onMounted(reload)
// Without this, the timer survives unmounting: the shell changes page, the
// component is destroyed, and a `reload()` keeps running every second against
// a dead component.
onUnmounted(stopProbe)

/**
 * Re-reads the state when the player changes, so that the highlighted track follows.
 *
 * The highlight comes from `index`, which only `api/data` carries — and the
 * probe stops as soon as no work is in progress. Since the current track
 * changes by itself at every end of track, the highlight thus stayed frozen on
 * the one from the start.
 *
 * A pushed stream rather than a permanent probe: the core already announces
 * every change, and probing continuously would hammer the plugin as long as a
 * tab stays open. One more re-read at the moment of the change, and nothing in
 * between.
 */
let closePlayer: (() => void) | null = null

/**
 * Name under which this plugin is served, derived from `base`.
 *
 * Derived and not hard-coded: it comes from `plugins.toml`, hence from the
 * deployment. `base` **is** that information, there is nothing to rebuild.
 */
const pluginName = computed(() => props.base.replace(/^\/plugins\//, '').replace(/\/+$/, ''))

/**
 * Is this source the one the core is playing, according to the pushed stream.
 *
 * This is the **core's** truth, and it cannot drift — unlike the flag the
 * plugin held, which could stay false after a start where mpv briefly goes idle
 * before loading the first file. Both are consulted together (see
 * `PlaylistPane`), to also cover the case where `EventSource` is unavailable.
 */
const activeSource = ref<string | null>(null)
const isActiveSource = computed(() => activeSource.value === pluginName.value)

onMounted(() => {
  closePlayer = onPlayer((state) => {
    const s = (state as { source?: unknown } | null)?.source
    activeSource.value = typeof s === 'string' ? s : null
    // Not during a send: the SDK serves requests serially, and a re-read added
    // on top would exceed the core's 5 s cap.
    if (!inProgress.value && !loadFailed.value) void reload()
  })
})
onUnmounted(() => closePlayer?.())

// Single flight: the SDK serves admin requests strictly serially, and the core
// gives up after 5 s. Two operations triggered back to back would queue up, the
// second exceeding the cap — the core would answer with the translated sentence
// from its catalog (`plugin_timeout`) for a perfectly legitimate action.
/** A send is in flight: this is what forbids the double send. */
const sending = ref(false)
/** A re-read follows a send: the UI stays greyed out, but a watcher is allowed
 * to emit — see the comment of `send`. */
const reloading = ref(false)
/** What greys out the UI: one or the other. */
const inProgress = computed(() => sending.value || reloading.value)

/**
 * Sends an operation, then re-reads the state.
 *
 * A refusal arrives in the form `{"error": "<already translated sentence>"}`:
 * the sentence is produced by the server's i18n catalogs and displayed **as
 * is**. In particular, the failure of `{"op":"mount"}` carries the output of
 * `systemctl`: that is what is actionable, rewording it would destroy it.
 */
async function send(payload: Record<string, unknown>): Promise<Data | null> {
  // Belt and braces: the protection does not rest on the buttons' `disabled`
  // alone, which a development tool or a future reshuffle of the template could
  // bypass — while the consequence (overwriting `media-roots.toml` with an
  // empty table) is irreversible.
  //
  // The flight only covers **the send**, not the re-read that follows. The
  // reload updates `data`, which triggers Vue's render flush, hence the panes'
  // watchers — including the one that loads the first level of the tree when
  // the roots change. As long as the flight also covered the re-read, that
  // watcher called `send` while the lock was still held: it received `null`,
  // and nothing relaunched it afterwards (the probe is only armed during a
  // scan). Symptom measured in the e2e journey: after saving a root, the
  // Browse pane stayed hopelessly empty.
  if (loadFailed.value || sending.value) return null
  sending.value = true
  let err: string | null
  try {
    err = await api.put(url('api/data'), payload)
  } finally {
    // In a `finally`: an exception must not leave the page stuck on a flight
    // that no longer exists.
    sending.value = false
  }
  if (err) {
    message.value = err
    return null
  }
  message.value = ''
  // `reloading` replaces the flight as far as greying out the UI goes: the
  // buttons stay inert for the duration of the re-read, without preventing a
  // watcher from emitting its own send.
  reloading.value = true
  try {
    await reload()
  } finally {
    reloading.value = false
  }
  return data.value
}

const scan = computed(
  () => data.value?.scan ?? { running: false, found: 0, dir: '', error: '' },
)
</script>

<template>
  <div class="space-y-8">
    <!-- The message is rendered in a `<pre>`: the failure of a mount carries
         the raw output of `systemctl`, over several lines. A `<p>` would fold
         it into an unreadable paragraph, and yet it is the only actionable
         thing the user receives. -->
    <pre
      v-if="message && !popoverOpen"
      data-message
      class="whitespace-pre-wrap rounded-md border border-border bg-muted/40 p-2 font-mono text-sm"
      >{{ message }}</pre
    >

    <!-- Scan progress. It only appears during an `add_dir`, and it is the
         probe — not a notification from the plugin — that makes it advance. -->
    <p v-if="scan.running" data-scan class="text-sm text-muted-foreground">
      {{ t('scan_progress', { found: scan.found, dir: scan.dir }) }}
    </p>

    <!-- Progress of the duration survey. Saying it rather than letting the
         column fill itself in: on a slow share, a list that changes before
         one's eyes without explanation is worrying. -->
    <p
      v-if="data?.durations.running"
      data-durations
      class="text-sm text-muted-foreground"
    >
      {{
        t('duration_progress', {
          done: data.durations.done,
          total: data.durations.total,
        })
      }}
    </p>

    <!-- Incident of the **last** scan, already translated by the plugin and
         displayed verbatim. It survives the end of the scan, and it is the only
         place where the page can learn that an addition failed: `add_dir`
         returns long before the end of the recursive walk, so its
         acknowledgement says nothing about its outcome. -->
    <pre
      v-if="scan.error"
      data-scan-error
      class="whitespace-pre-wrap rounded-md border border-destructive p-2 font-mono text-sm"
      >{{ scan.error }}</pre
    >

    <!-- Three tabs rather than three panes end to end: the page required a
         long scroll to reach the declaration of a source, a rare gesture,
         whereas the playlist and the browser are the two screens one really
         opens.

         `force-mount` everywhere, and it is not a detail: without it the
         inactive panels would be unmounted, so that coming back to "Browse"
         after a detour would reopen the root of the source instead of the
         folder we were in — and would relaunch a `browse` at every back and
         forth. The panes therefore stay alive, only the display changes. -->
    <!-- The wait. `role="status"` carries the only text; the blocks are
         `aria-hidden`, so a screen reader hears it announced once rather than
         a run of empty boxes. The shape stands in for the tab strip and the
         list under it. -->
    <div v-if="skeleton" role="status" class="space-y-4">
      <span class="sr-only">{{ t('loading') }}</span>
      <Skeleton class="h-9 w-72" />
      <div class="space-y-2">
        <Skeleton v-for="i in 6" :key="i" class="h-8 w-full" />
      </div>
    </div>

    <Tabs v-else-if="data" default-value="playlist">
      <TabsList>
        <!-- `data-tab` carries the value and not only the marker: the
             end-to-end journey must designate a tab without depending on its
             label, which is translated. -->
        <TabsTrigger value="playlist" data-tab="playlist">{{ t('playlist_title') }}</TabsTrigger>
        <TabsTrigger value="browse" data-tab="browse">
          {{ t('browse_title') }}
        </TabsTrigger>
        <TabsTrigger value="sources" data-tab="sources">{{ t('sources_title') }}</TabsTrigger>
      </TabsList>

      <TabsContent value="playlist" force-mount>
        <PlaylistPane
          :data="data"
          :t="t"
          :send="send"
          :frozen="loadFailed || inProgress"
          :is-active-source="isActiveSource"
        />
      </TabsContent>
      <TabsContent value="browse" force-mount>
        <BrowsePane
          :data="data"
          :t="t"
          :send="send"
          :frozen="loadFailed || inProgress"
        />
      </TabsContent>
      <TabsContent value="sources" force-mount>
        <SourcesPane
          :data="data"
          :t="t"
          :send="send"
          :frozen="loadFailed || inProgress"
          :message="message"
        />
      </TabsContent>
    </Tabs>
  </div>
</template>
