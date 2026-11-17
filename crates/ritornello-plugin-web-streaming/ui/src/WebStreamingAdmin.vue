<script setup lang="ts">
import {
  api, Button, Card, CardAction, CardContent, CardHeader, CardTitle, createT, Skeleton, useSkeleton, type Catalog,
} from '@ritornello/ui'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import AddLinksDialog from './AddLinksDialog.vue'
import CapabilitiesPanel from './CapabilitiesPanel.vue'
import { emptyData, workInProgress, type Choices, type Data, type Entry, type Target } from './data'
import DownloadDialog, { type Item } from './DownloadDialog.vue'
import EntryList from './EntryList.vue'
import JobList from './JobList.vue'
import ToolPanel from './ToolPanel.vue'

// `base` is part of the plugin UI contract, like `catalog`: the **absolute**
// prefix under which the core serves this plugin's routes. Required, with no
// default: the name a plugin is served under comes from the deployment.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

function url(path: string): string {
  return `${props.base}${path}`
}

/** How often the page reads again while something runs. */
const PROBE_PERIOD_MS = 1000
/** Reads after an action whose effect lands a moment later (a setting written by a task). */
const GRACE_READS = 3
/**
 * While no share is offered, ask again this often: a share mounted while the
 * page sits idle (the files plugin mounts them after this one starts) must
 * appear without a reload.
 */
const TARGETS_RETRY_MS = 10_000

const data = ref<Data | null>(null)
/** Addresses of the presets ticked: a number changes under a move, an address does not. */
const selected = ref<Set<string>>(new Set())
const message = ref('')
const dialogOpen = ref(false)

/**
 * Set when the first load failed, and never lifted (only a reload of the page
 * recovers). After a failed GET the table shown is empty while the server's is
 * not: better an inert page than one that acts on what it failed to read.
 */
const loadFailed = ref(false)
const loaded = ref(false)
const skeleton = useSkeleton(() => !loaded.value)

/** The server serves them in order; the page does not rely on it. */
function byPreset(entries: Entry[]): Entry[] {
  return [...entries].sort((a, b) => a.preset - b.preset)
}

// ---- reading, and polling while work runs -----------------------------------
//
// The admin protocol pushes nothing: everything slow on the plugin side (a
// probe, a download, an update, a look at a share) only reaches the screen
// through this read, repeated for as long as something runs.

let timer: ReturnType<typeof setTimeout> | null = null
let grace = 0

function stopProbe(): void {
  if (timer !== null) {
    clearTimeout(timer)
    timer = null
  }
}

function scheduleProbe(): void {
  stopProbe()
  if (!workInProgress(data.value) && grace <= 0) return
  grace -= 1
  timer = setTimeout(() => void reload(), PROBE_PERIOD_MS)
}

async function reload(): Promise<void> {
  try {
    const d = await api.get<Data>(url('api/data'))
    data.value = d
    // A preset that went (removed here, or from another page) is no longer ticked.
    const urls = new Set(d.entries.map((e) => e.url))
    if ([...selected.value].some((u) => !urls.has(u))) {
      selected.value = new Set([...selected.value].filter((u) => urls.has(u)))
    }
    scheduleProbe()
  } catch (e) {
    // A later failed read does not make the page inert: the data it holds is
    // still true. The first one does (see `loadFailed`).
    message.value = t.value('load_error', { cause: (e as Error).message })
    if (!loaded.value) {
      loadFailed.value = true
      data.value = emptyData()
    }
    // While work runs, a failed read is no reason to stop reading: the
    // plugin may be restarting, and what runs is still running.
    scheduleProbe()
  } finally {
    loaded.value = true
  }
}

/**
 * Asks the plugin to look at its shares again, then reads once more when it
 * had the time to. The shares are not known for good at start: the files
 * plugin mounts them after this one is up, and one can go at any time.
 */
async function refreshTargets(): Promise<void> {
  await send({ op: 'list_targets' }, 1)
}

let retryTimer: ReturnType<typeof setInterval> | null = null

onMounted(async () => {
  retryTimer = setInterval(() => {
    if (noShare.value && !document.hidden) void refreshTargets()
  }, TARGETS_RETRY_MS)
  await reload()
  await refreshTargets()
})
onBeforeUnmount(() => {
  stopProbe()
  if (retryTimer !== null) clearInterval(retryTimer)
})

/** Sends an operation, then reads at once and keeps reading a few times. */
async function send(op: Record<string, unknown>, reads = GRACE_READS): Promise<string | null> {
  if (loadFailed.value) return null
  const err = await api.put(url('api/data'), op)
  grace = reads
  await reload()
  return err
}

// ---- the presets ------------------------------------------------------------
//
// The server's table is the only one: every action is sent at once, by the
// preset's number and address, and the page reads again. A `stale` refusal
// (the table moved meanwhile) needs nothing more than that read.

const entries = computed(() => byPreset(data.value?.entries ?? []))

async function act(op: Record<string, unknown>): Promise<void> {
  const err = await send(op)
  message.value = err ?? ''
}

function remove(preset: number, url: string): Promise<void> {
  return act({ op: 'remove', preset, url })
}

function moveTo(preset: number, url: string, to: number): Promise<void> {
  return act({ op: 'move', preset, url, to })
}

function rename(preset: number, url: string, title: string): Promise<void> {
  return act({ op: 'rename', preset, url, title })
}

function toggle(url: string): void {
  if (selected.value.has(url)) selected.value.delete(url)
  else selected.value.add(url)
}

// ---- links ------------------------------------------------------------------

const addOpen = ref(false)
const adding = ref(false)
const addError = ref('')
/** Changed to empty the dialog's field once the links were accepted. */
const addKey = ref(0)

function openAdd(): void {
  addError.value = ''
  addOpen.value = true
}

/** The links are queued by the plugin: on its `Ok` the dialog closes, on a refusal it stays open on the reason. */
async function probe(text: string): Promise<void> {
  // `send` answers null when it sent nothing (the page is inert): that is not an `Ok`.
  if (loadFailed.value) return
  adding.value = true
  try {
    const err = await send({ op: 'probe', text })
    if (err) {
      addError.value = err
      return
    }
    addOpen.value = false
    addError.value = ''
    addKey.value += 1
  } finally {
    adding.value = false
  }
}

function choose(line: number, url: string): Promise<void> {
  return act({ op: 'choose', line, url })
}

// ---- downloading ------------------------------------------------------------

const noShare = computed(() => (data.value?.targets.length ?? 0) === 0)
const downloadBlocked = computed(() => noShare.value || selected.value.size === 0)

function openDialog(): void {
  // Before the early return: asking is what makes a share that just
  // appeared usable.
  void refreshTargets()
  if (downloadBlocked.value) return
  dialogOpen.value = true
}

async function listDirs(target: Target): Promise<void> {
  const err = await send({ op: 'list_dirs', target })
  if (err) message.value = err
}

async function createDir(target: Target, name: string): Promise<void> {
  const err = await send({ op: 'create_dir', target, name })
  if (err) message.value = err
}

/** The presets ticked for download, in order: what the dialog names. */
const chosen = computed<Item[]>(() =>
  entries.value
    .filter((e) => selected.value.has(e.url))
    .map((e) => ({ preset: e.preset, url: e.url, title: e.title, kind: e.kind })),
)

async function download(target: Target, choices: Choices): Promise<void> {
  const items = chosen.value.map(({ preset, url }) => ({ preset, url }))
  const err = await send({ op: 'download', items, target, ...choices })
  message.value = err ?? ''
  if (!err) {
    dialogOpen.value = false
    selected.value = new Set()
  }
}

async function cancel(job: number): Promise<void> {
  const err = await send({ op: 'cancel', job })
  if (err) message.value = err
}

/** Whether any download is over: only those are cleared. */
const anyOver = computed(() => (data.value?.jobs ?? []).some((j) => j.state !== 'queued' && j.state !== 'running'))

async function clearJobs(): Promise<void> {
  const err = await send({ op: 'clear_jobs' })
  if (err) message.value = err
}

// ---- yt-dlp -----------------------------------------------------------------

async function updateTool(force: boolean): Promise<void> {
  const err = await send({ op: 'update_tool', force })
  message.value = err ?? ''
}

async function setChannel(nightly: boolean): Promise<void> {
  const err = await send({ op: 'set_channel', channel: nightly ? 'nightly' : 'stable' })
  message.value = err ?? ''
}

async function recheck(): Promise<void> {
  const err = await send({ op: 'reprobe' })
  message.value = err ?? ''
}
</script>

<template>
  <div class="space-y-6">
    <div v-if="skeleton" role="status" class="space-y-2">
      <span class="sr-only">{{ t('loading') }}</span>
      <Skeleton v-for="i in 4" :key="i" class="h-10 w-full" />
    </div>

    <template v-else-if="loaded && data">
      <Card>
        <CardHeader>
          <CardTitle>{{ t('entries_title') }}</CardTitle>
          <CardAction>
            <Button variant="outline" size="sm" data-add-links :disabled="loadFailed" @click="openAdd">
              {{ t('btn_add_links') }}
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent class="space-y-3">
          <EntryList
            :t="t"
            :entries="entries"
            :lines="data.probe.results"
            :selected="selected"
            :frozen="loadFailed"
            :download-blocked="downloadBlocked"
            :no-share="noShare"
            @remove="remove"
            @move="moveTo"
            @rename="rename"
            @toggle="toggle"
            @choose="choose"
            @dismiss="(line) => act({ op: 'dismiss', line })"
            @download="openDialog"
          />
          <p v-if="message" class="text-sm text-muted-foreground" role="status" data-message>{{ message }}</p>
        </CardContent>
      </Card>

      <!-- Right under the presets: a download is started there, and its
           progress is read here without scrolling past the other cards. -->
      <Card data-jobs-card>
        <CardHeader>
          <CardTitle>{{ t('jobs_title') }}</CardTitle>
          <CardAction v-if="anyOver">
            <Button variant="outline" size="sm" data-jobs-clear :disabled="loadFailed" @click="clearJobs">
              {{ t('btn_clear_jobs') }}
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent>
          <JobList :t="t" :jobs="data.jobs" @cancel="cancel" />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{{ t('tool_title') }}</CardTitle>
        </CardHeader>
        <CardContent class="space-y-4">
          <ToolPanel
            :t="t"
            :tool="data.tool"
            :frozen="loadFailed"
            :last-failure="data.last_failure ?? null"
            @update="updateTool"
            @channel="setChannel"
          />
          <CapabilitiesPanel :t="t" :caps="data.capabilities" :frozen="loadFailed" @recheck="recheck" />
        </CardContent>
      </Card>
    </template>

    <AddLinksDialog
      v-if="data"
      :key="addKey"
      :t="t"
      :open="addOpen"
      :busy="adding"
      :frozen="loadFailed"
      :error="addError"
      @update:open="addOpen = $event"
      @submit="probe"
    />

    <DownloadDialog
      v-if="data"
      :t="t"
      :open="dialogOpen"
      :targets="data.targets"
      :last-target="data.last_target"
      :explore="data.explore"
      :items="chosen"
      :default-names="data.default_names ?? {}"
      :last-format="data.last_format ?? 'original'"
      :last-cover="data.last_cover ?? 'separate'"
      :ffmpeg="data.capabilities.ffmpeg ?? null"
      :ffmpeg-hint="data.capabilities.ffmpeg_hint ?? null"
      @update:open="dialogOpen = $event"
      @list="listDirs"
      @create="createDir"
      @confirm="download"
    />
  </div>
</template>
