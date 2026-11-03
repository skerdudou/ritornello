<script setup lang="ts">
import {
  Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, Input,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { faultText, targetText, type Explore, type T, type Target } from './data'

const props = defineProps<{
  t: T
  open: boolean
  /** The shares a download may go to. */
  targets: string[]
  /** What the last download used: preselected while its share is still there. */
  lastTarget: Target | null
  explore: Explore
}>()

const emit = defineEmits<{
  'update:open': [open: boolean]
  list: [target: Target]
  create: [target: Target, name: string]
  confirm: [target: Target]
}>()

const root = ref('')
const path = ref<string[]>([])
const newName = ref('')

const current = computed<Target>(() => ({ root: root.value, path: path.value.join('/') }))

/** The listing of the folder the dialog is on, not that of one it left. */
const names = computed(() => {
  const l = props.explore.listing
  if (!l || l.target.root !== current.value.root || l.target.path !== current.value.path) return null
  return l.names
})

function look(): void {
  newName.value = ''
  emit('list', current.value)
}

// Opened: the last target when its share is still offered, else the first
// share. `lastTarget` only ever names a share that is in `targets` (the
// plugin drops it otherwise), but the page does not rely on that alone.
watch(
  () => props.open,
  (open) => {
    if (!open || !props.targets.length) return
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
  emit('create', current.value, name)
  newName.value = ''
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{{ t('dlg_download_title') }}</DialogTitle>
        <DialogDescription>{{ t('dlg_download_desc') }}</DialogDescription>
      </DialogHeader>

      <div class="space-y-3">
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
        <p v-else-if="names === null" class="text-sm text-muted-foreground" role="status">{{ t('loading') }}</p>
        <p v-else-if="!names.length" class="text-sm text-muted-foreground">{{ t('no_folders') }}</p>
        <ul v-else class="max-h-56 space-y-1 overflow-y-auto">
          <li v-for="n in names" :key="n">
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

        <div class="flex justify-end gap-2">
          <Button variant="ghost" data-cancel @click="emit('update:open', false)">{{ t('btn_cancel') }}</Button>
          <Button data-download-here :disabled="!root" @click="emit('confirm', current)">
            {{ t('btn_download_here') }}
          </Button>
        </div>
      </div>
    </DialogContent>
  </Dialog>
</template>
