<script setup lang="ts">
import {
  api, Card, CardContent, CardHeader, CardTitle, createT, move, Skeleton, useSkeleton, type Catalog,
} from '@ritornello/ui'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import AddLinks from './AddLinks.vue'
import CapabilitiesPanel from './CapabilitiesPanel.vue'
import { emptyData, workInProgress, type Choices, type Data, type Entry, type Found, type Target } from './data'
import DownloadDialog, { type Item } from './DownloadDialog.vue'
import EntryList, { type Row } from './EntryList.vue'
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

/** The server's bound (1..=99); `save` there is the authority. */
const MAX = 99
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
const rows = ref<Row[]>([])
const selected = ref<Set<number>>(new Set())
const message = ref('')
const dialogOpen = ref(false)

/**
 * Set when the first load failed, and never lifted (only a reload of the page
 * recovers). After a failed GET the table is empty while the server's is not:
 * a "Save" would overwrite every preset with nothing. Better an inert page
 * than one that destroys what it failed to read.
 */
const loadFailed = ref(false)
const loaded = ref(false)
const skeleton = useSkeleton(() => !loaded.value)

let nextKey = 0
function rowOf(
  e: {
    url: string; kind: Row['kind']; title: string; channel?: string | null; count?: number | null
    site?: string | null; identity?: string | null
  },
  saved: number | null,
): Row {
  nextKey += 1
  return {
    key: nextKey, saved, url: e.url, kind: e.kind, title: e.title, channel: e.channel ?? null, count: e.count ?? null,
    site: e.site ?? null, identity: e.identity ?? null,
  }
}

/** The server serves them in order; the page does not rely on it. */
function byPreset(entries: Entry[]): Entry[] {
  return [...entries].sort((a, b) => a.preset - b.preset)
}

/** The rows become what the server holds: at load, and after a save. */
function adopt(d: Data): void {
  rows.value = byPreset(d.entries).map((e) => rowOf(e, e.preset))
  selected.value = new Set()
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

/** `adoptRows`: the table becomes what the server holds (first read, after a save). */
async function reload(adoptRows = !loaded.value): Promise<void> {
  try {
    const d = await api.get<Data>(url('api/data'))
    data.value = d
    if (adoptRows) adopt(d)
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
  await reload(false)
  return err
}

// ---- the presets ------------------------------------------------------------

const dirty = computed(() => {
  const server = byPreset(data.value?.entries ?? [])
  return (
    rows.value.length !== server.length ||
    rows.value.some((r, i) => {
      const s = server[i]!
      return r.saved !== s.preset || r.url !== s.url || r.title !== s.title
    })
  )
})

const present = computed(() => new Set(rows.value.map((r) => r.url)))

function reorder(from: number, to: number): void {
  rows.value = move(rows.value, from, to)
}

function remove(i: number): void {
  const [gone] = rows.value.splice(i, 1)
  if (gone) selected.value.delete(gone.key)
}

function toggle(key: number): void {
  if (selected.value.has(key)) selected.value.delete(key)
  else selected.value.add(key)
}

function add(f: Found): void {
  if (rows.value.length >= MAX) {
    message.value = t.value('limit_reached')
    return
  }
  rows.value.push(rowOf(f, null))
  message.value = ''
}

async function save(): Promise<void> {
  if (loadFailed.value) return
  const entries = rows.value.map((r, i) => ({
    preset: i + 1, url: r.url, kind: r.kind, title: r.title, channel: r.channel, count: r.count,
    site: r.site, identity: r.identity,
  }))
  const err = await api.put(url('api/data'), { op: 'save', entries })
  if (err) {
    message.value = t.value('save_error', { cause: err })
    return
  }
  message.value = t.value('saved')
  // What was saved is what the server holds now: adopt it, so the rows carry
  // their new numbers (a download goes by number).
  await reload(true)
}

// ---- links ------------------------------------------------------------------

async function probe(text: string): Promise<void> {
  const err = await send({ op: 'probe', text })
  message.value = err ?? ''
}

// ---- downloading ------------------------------------------------------------

const noShare = computed(() => (data.value?.targets.length ?? 0) === 0)
const downloadBlocked = computed(() => noShare.value || selected.value.size === 0 || dirty.value)

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

/** The saved presets ticked for download, in order: what the dialog names. */
const chosen = computed<Item[]>(() =>
  rows.value
    .filter((r) => selected.value.has(r.key) && r.saved !== null)
    .map((r) => ({ preset: r.saved!, title: r.title, kind: r.kind })),
)

async function download(target: Target, choices: Choices): Promise<void> {
  const presets = chosen.value.map((i) => i.preset)
  const err = await send({ op: 'download', presets, target, ...choices })
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
        </CardHeader>
        <CardContent class="space-y-3">
          <EntryList
            :t="t"
            :rows="rows"
            :selected="selected"
            :dirty="dirty"
            :frozen="loadFailed"
            :download-blocked="downloadBlocked"
            :no-share="noShare"
            @reorder="reorder"
            @remove="remove"
            @toggle="toggle"
            @save="save"
            @download="openDialog"
          />
          <p v-if="message" class="text-sm text-muted-foreground" role="status" data-message>{{ message }}</p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{{ t('add_title') }}</CardTitle>
        </CardHeader>
        <CardContent>
          <AddLinks
            :t="t"
            :running="data.probe.running"
            :results="data.probe.results"
            :present="present"
            :frozen="loadFailed"
            @probe="probe"
            @add="add"
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{{ t('jobs_title') }}</CardTitle>
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
