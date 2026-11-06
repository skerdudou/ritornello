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

/**
 * Two steps: where (the share and the folder), then how (format, cover,
 * names). Browsing and choosing are apart, each with the room it needs.
 */
const step = ref<'where' | 'options'>('where')

const hasPlaylist = computed(() => props.items.some((i) => i.kind === 'playlist'))

function look(): void {
  newName.value = ''
  creating.value = null
  emit('list', current.value)
}

// Opened: on the first step, at the last target when its share is still offered, else the first
// share. `lastTarget` only ever names a share that is in `targets` (the
// plugin drops it otherwise), but the page does not rely on that alone. The
// format and cover are the last ones (mp3 only while ffmpeg is there), the
// names the plugin's.
watch(
  () => props.open,
  (open) => {
    if (!open) return
    step.value = 'where'
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
const COVERS: CoverMode[] = ['embedded', 'separate']

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
        <DialogDescription data-step :data-step-name="step">
          {{ step === 'where' ? t('dlg_step_where') : t('dlg_step_options') }}
        </DialogDescription>
      </DialogHeader>

      <!-- Step 1: where. The share, then one framed browser — the path on
           top, the folders, and the new folder at its foot — so that what
           is being browsed reads as one block, apart from everything else. -->
      <div v-if="step === 'where'" class="max-h-[65vh] space-y-4 overflow-y-auto" data-step-where>
        <section class="space-y-2">
          <h3 class="text-sm font-medium">{{ t('share_label') }}</h3>
          <div class="flex flex-wrap gap-2">
            <Button
              v-for="name in targets"
              :key="name"
              size="sm"
              :variant="name === root ? 'default' : 'outline'"
              :aria-pressed="name === root"
              :data-share="name"
              @click="chooseShare(name)"
            >
              {{ name }}
            </Button>
          </div>
        </section>

        <section class="space-y-2">
          <h3 class="text-sm font-medium">{{ t('folders_label') }}</h3>
          <div class="overflow-hidden rounded-md border">
            <div class="flex items-center gap-2 border-b bg-muted/50 px-2 py-1.5 text-sm">
              <Button
                variant="ghost"
                size="sm"
                data-up
                :disabled="!path.length"
                :title="t('btn_up')"
                :aria-label="t('btn_up')"
                @click="up"
              >
                ↑
              </Button>
              <span class="min-w-0 truncate font-medium" data-where :title="targetText(current)">
                {{ path.length ? targetText(current) : `${root} — ${t('folder_top')}` }}
              </span>
            </div>

            <div class="max-h-56 min-h-24 overflow-y-auto p-1">
              <p v-if="explore.error" class="p-2 text-sm text-destructive" role="alert" data-explore-error>
                {{ faultText(t, explore.error) }}
              </p>
              <p v-else-if="listed === null" class="p-2 text-sm text-muted-foreground" role="status">{{ t('loading') }}</p>
              <p v-else-if="!listed.length" class="p-2 text-sm text-muted-foreground">{{ t('no_folders') }}</p>
              <ul v-else class="space-y-0.5">
                <li v-for="n in listed" :key="n">
                  <Button variant="ghost" size="sm" class="w-full justify-start" :data-dir="n" @click="descend(n)">
                    <span aria-hidden="true">📁</span>
                    <span class="truncate">{{ n }}</span>
                  </Button>
                </li>
              </ul>
            </div>

            <div class="flex items-center gap-2 border-t p-2">
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
          </div>
          <!-- Always shown, one line: a folder with a `$` is not offered, and
               the operator who made one by hand elsewhere would look for it
               here. -->
          <p class="text-xs text-muted-foreground" data-dollar-note>{{ t('dollar_note') }}</p>
        </section>
      </div>

      <!-- Step 2: how. The destination stays in sight, one click from being
           changed; each choice has its label above its buttons, which share
           the width equally instead of wrapping one by one. -->
      <div v-else class="max-h-[65vh] space-y-4 overflow-y-auto" data-step-options>
        <div class="flex items-center gap-2 rounded-md border bg-muted/50 px-3 py-2 text-sm">
          <span class="text-muted-foreground">{{ t('destination_label') }}</span>
          <span class="min-w-0 flex-1 truncate font-medium" data-destination :title="targetText(current)">
            {{ path.length ? targetText(current) : `${root} — ${t('folder_top')}` }}
          </span>
          <Button variant="ghost" size="sm" data-change-target @click="step = 'where'">{{ t('btn_change') }}</Button>
        </div>

        <section class="space-y-2">
          <h3 class="text-sm font-medium">{{ t('format_label') }}</h3>
          <div class="grid grid-cols-3 gap-2">
            <Button
              v-for="f in FORMATS"
              :key="f"
              size="sm"
              class="w-full"
              :variant="f === format ? 'default' : 'outline'"
              :aria-pressed="f === format"
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
        </section>

        <section class="space-y-2" data-cover-choice>
          <h3 class="text-sm font-medium">{{ t('cover_label') }}</h3>
          <div class="grid grid-cols-2 gap-2">
            <Button
              v-for="c in COVERS"
              :key="c"
              size="sm"
              class="w-full"
              :variant="cover === c ? 'default' : 'outline'"
              :aria-pressed="cover === c"
              :data-cover="c"
              @click="cover = c"
            >
              {{ c === 'embedded' ? t('cover_embedded') : t('cover_separate') }}
            </Button>
          </div>
          <p class="text-xs text-muted-foreground" data-cover-help>
            {{ hasPlaylist ? t('cover_help_playlist') : t('cover_help') }}
          </p>
        </section>

        <section v-if="items.length" class="space-y-2">
          <h3 class="text-sm font-medium">{{ t('names_label') }}</h3>
          <label v-for="i in items" :key="i.preset" class="block space-y-1 text-sm">
            <span class="block truncate text-muted-foreground">{{ i.title }}</span>
            <Input
              v-model="names[String(i.preset)]"
              :data-name="i.preset"
              :placeholder="i.kind === 'video' ? t('name_video_hint') : t('name_playlist_hint')"
              :title="i.kind === 'video' ? t('name_video_hint') : t('name_playlist_hint')"
            />
          </label>
        </section>
      </div>

      <div class="flex justify-end gap-2">
        <template v-if="step === 'where'">
          <Button variant="ghost" data-cancel @click="emit('update:open', false)">{{ t('btn_cancel') }}</Button>
          <Button data-next :disabled="!root" @click="step = 'options'">{{ t('btn_next') }}</Button>
        </template>
        <template v-else>
          <Button variant="ghost" data-back @click="step = 'where'">{{ t('btn_back') }}</Button>
          <Button data-download-here :disabled="!root" @click="confirm">
            {{ t('btn_download_here') }}
          </Button>
        </template>
      </div>
    </DialogContent>
  </Dialog>
</template>
