<script setup lang="ts">
import {
  Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, Input,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import {
  faultText, targetText, type Choices, type CoverMode, type Explore, type Format, type Kind, type T, type Target,
} from './data'

/** A preset about to be downloaded, as the dialog names it. */
export interface Item {
  preset: number
  title: string
  kind: Kind
}

const props = defineProps<{
  t: T
  open: boolean
  /** The shares a download may go to. */
  targets: string[]
  /** What the last download used: preselected while its share is still there. */
  lastTarget: Target | null
  explore: Explore
  /** The presets selected, in order. */
  items: Item[]
  /** The name the plugin would give each preset, by its number. */
  defaultNames: Record<string, string>
  lastFormat: Format
  lastCover: CoverMode
  /** ffmpeg's version, null without it: mp3 is then not offered. */
  ffmpeg: string | null
  /** The command that installs ffmpeg. */
  ffmpegHint: string | null
}>()

const emit = defineEmits<{
  'update:open': [open: boolean]
  list: [target: Target]
  create: [target: Target, name: string]
  confirm: [target: Target, choices: Choices]
}>()

const root = ref('')
const path = ref<string[]>([])
const newName = ref('')
const format = ref<Format>('original')
const cover = ref<CoverMode>('separate')
const names = ref<Record<string, string>>({})

/**
 * A folder asked for and not yet confirmed: the dialog enters it once the
 * plugin lists it in its parent (the creation went through), never before.
 */
const creating = ref<{ target: Target; name: string } | null>(null)

const current = computed<Target>(() => ({ root: root.value, path: path.value.join('/') }))

/** The listing of the folder the dialog is on, not that of one it left. */
const listed = computed(() => {
  const l = props.explore.listing
  if (!l || l.target.root !== current.value.root || l.target.path !== current.value.path) return null
  return l.names
})

const hasVideo = computed(() => props.items.some((i) => i.kind === 'video'))

function look(): void {
  newName.value = ''
  creating.value = null
  emit('list', current.value)
}

// Opened: the last target when its share is still offered, else the first
// share. `lastTarget` only ever names a share that is in `targets` (the
// plugin drops it otherwise), but the page does not rely on that alone. The
// format and cover are the last ones (mp3 only while ffmpeg is there), the
// names the plugin's.
watch(
  () => props.open,
  (open) => {
    if (!open) return
    format.value = props.lastFormat === 'mp3' && !props.ffmpeg ? 'original' : props.lastFormat
    cover.value = props.lastCover
    names.value = Object.fromEntries(props.items.map((i) => [String(i.preset), props.defaultNames[String(i.preset)] ?? '']))
    if (!props.targets.length) return
    const last = props.lastTarget
    if (last && props.targets.includes(last.root)) {
      root.value = last.root
      path.value = last.path ? last.path.split('/') : []
    } else {
      root.value = props.targets[0]!
      path.value = []
    }
    look()
  },
  { immediate: true },
)

// The folder just created: entered once its parent's listing names it, which
// is the plugin's confirmation. A refusal (an error, or a listing without
// it) forgets it.
watch(
  () => props.explore,
  (e) => {
    const c = creating.value
    if (!c || e.busy) return
    if (e.error) {
      creating.value = null
      return
    }
    const l = e.listing
    if (!l || l.target.root !== c.target.root || l.target.path !== c.target.path) return
    if (current.value.root !== c.target.root || current.value.path !== c.target.path) return
    creating.value = null
    if (l.names.includes(c.name)) descend(c.name)
  },
  { deep: true },
)

function chooseShare(name: string): void {
  root.value = name
  path.value = []
  look()
}

function descend(name: string): void {
  path.value = [...path.value, name]
  look()
}

function up(): void {
  path.value = path.value.slice(0, -1)
  look()
}

function create(): void {
  const name = newName.value.trim()
  if (!name) return
  creating.value = { target: { ...current.value }, name }
  emit('create', current.value, name)
  newName.value = ''
}

function confirm(): void {
  const chosen: Record<string, string> = {}
  for (const i of props.items) chosen[String(i.preset)] = names.value[String(i.preset)] ?? ''
  emit('confirm', current.value, { format: format.value, cover: cover.value, names: chosen })
}

const FORMATS: Format[] = ['original', 'm4a', 'mp3']

function formatLabel(f: Format): string {
  switch (f) {
    case 'm4a':
      return props.t('format_m4a')
    case 'mp3':
      return props.t('format_mp3')
    default:
      return props.t('format_original')
  }
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{{ t('dlg_download_title') }}</DialogTitle>
        <DialogDescription>{{ t('dlg_download_desc') }}</DialogDescription>
      </DialogHeader>

      <div class="max-h-[70vh] space-y-3 overflow-y-auto">
        <div class="flex flex-wrap items-center gap-2">
          <span class="text-sm text-muted-foreground">{{ t('share_label') }}</span>
          <Button
            v-for="name in targets"
            :key="name"
            size="sm"
            :variant="name === root ? 'default' : 'outline'"
            :data-share="name"
            @click="chooseShare(name)"
          >
            {{ name }}
          </Button>
        </div>

        <div class="flex items-center gap-2 text-sm">
          <Button variant="ghost" size="sm" data-up :disabled="!path.length" @click="up">
            ↑ {{ t('btn_up') }}
          </Button>
          <span class="min-w-0 truncate text-muted-foreground" data-where :title="targetText(current)">
            {{ path.length ? targetText(current) : `${root} — ${t('folder_top')}` }}
          </span>
        </div>

        <p v-if="explore.error" class="text-sm text-destructive" role="alert" data-explore-error>
          {{ faultText(t, explore.error) }}
        </p>
        <p v-else-if="listed === null" class="text-sm text-muted-foreground" role="status">{{ t('loading') }}</p>
        <p v-else-if="!listed.length" class="text-sm text-muted-foreground">{{ t('no_folders') }}</p>
        <ul v-else class="max-h-56 space-y-1 overflow-y-auto">
          <li v-for="n in listed" :key="n">
            <Button variant="ghost" size="sm" class="w-full justify-start" :data-dir="n" @click="descend(n)">
              {{ n }}
            </Button>
          </li>
        </ul>
        <!-- Always shown, one line: a folder with a `$` is not offered, and the
             operator who made one by hand elsewhere would look for it here. -->
        <p class="text-xs text-muted-foreground" data-dollar-note>{{ t('dollar_note') }}</p>

        <div class="flex items-center gap-2">
          <Input
            v-model="newName"
            class="flex-1"
            data-new-folder
            :placeholder="t('new_folder_placeholder')"
            @keydown.enter="create"
          />
          <Button variant="secondary" data-create-folder :disabled="!newName.trim() || explore.busy" @click="create">
            {{ t('btn_new_folder') }}
          </Button>
        </div>

        <div class="space-y-1">
          <div class="flex flex-wrap items-center gap-2">
            <span class="text-sm text-muted-foreground">{{ t('format_label') }}</span>
            <Button
              v-for="f in FORMATS"
              :key="f"
              size="sm"
              :variant="f === format ? 'default' : 'outline'"
              :data-format="f"
              :disabled="f === 'mp3' && !ffmpeg"
              @click="format = f"
            >
              {{ formatLabel(f) }}
            </Button>
          </div>
          <p v-if="format === 'm4a'" class="text-xs text-muted-foreground" data-format-help>{{ t('format_m4a_help') }}</p>
          <p v-if="!ffmpeg" class="text-xs text-muted-foreground" data-mp3-unavailable>
            {{ t('format_mp3_unavailable', { command: ffmpegHint ?? '' }) }}
          </p>
        </div>

        <div v-if="hasVideo" class="flex flex-wrap items-center gap-2" data-cover-choice>
          <span class="text-sm text-muted-foreground">{{ t('cover_label') }}</span>
          <Button
            size="sm"
            :variant="cover === 'embedded' ? 'default' : 'outline'"
            data-cover="embedded"
            @click="cover = 'embedded'"
          >
            {{ t('cover_embedded') }}
          </Button>
          <Button
            size="sm"
            :variant="cover === 'separate' ? 'default' : 'outline'"
            data-cover="separate"
            @click="cover = 'separate'"
          >
            {{ t('cover_separate') }}
          </Button>
        </div>

        <div v-if="items.length" class="space-y-2">
          <span class="text-sm text-muted-foreground">{{ t('names_label') }}</span>
          <label v-for="i in items" :key="i.preset" class="block space-y-1 text-sm">
            <span class="block truncate">{{ i.title }}</span>
            <Input
              v-model="names[String(i.preset)]"
              :data-name="i.preset"
              :placeholder="i.kind === 'video' ? t('name_video_hint') : t('name_playlist_hint')"
              :title="i.kind === 'video' ? t('name_video_hint') : t('name_playlist_hint')"
            />
          </label>
        </div>

        <div class="flex justify-end gap-2">
          <Button variant="ghost" data-cancel @click="emit('update:open', false)">{{ t('btn_cancel') }}</Button>
          <Button data-download-here :disabled="!root" @click="confirm">
            {{ t('btn_download_here') }}
          </Button>
        </div>
      </div>
    </DialogContent>
  </Dialog>
</template>
