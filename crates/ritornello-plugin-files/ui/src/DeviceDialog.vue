<script setup lang="ts">
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Input,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import FolderPicker from './FolderPicker.vue'
import type { Data, Send, T } from './data'

/**
 * "Device folder" wizard.
 *
 * It opens on the list of volumes, never on `/`: nobody knows the absolute
 * path of a USB key, and that is precisely what the old form asked for
 * typing.
 */
const props = defineProps<{
  data: Data
  t: T
  send: Send
  frozen: boolean
  open: boolean
  /**
   * Last refusal from the plugin, as the page received it.
   *
   * It lands here because a refusal born inside the dialog must display
   * there: the main page's banner is behind the dialog's grey veil, hence
   * about invisible right when it matters most.
   */
  message: string
}>()
const emit = defineEmits<{ close: [] }>()

const ex = computed(() => props.data.explore)
/** A volume has been chosen: we are in the tree rather than in the list. */
const inTree = computed(() => ex.value.kind === 'local' && ex.value.path !== '')

/**
 * Volume containing the current path: the longest declared prefix.
 *
 * Used to bound going up. Without it, `goUp` from the top of a USB key would
 * lead into `/media`, then `/` — leaving the volume just chosen, without ever
 * finding the list again.
 */
const currentVolume = computed(() => {
  const under = (base: string) =>
    ex.value.path === base || ex.value.path.startsWith(`${base.replace(/\/$/, '')}/`)
  return (
    props.data.volumes
      .filter((v) => under(v.path))
      .sort((a, b) => b.path.length - a.path.length)[0] ?? null
  )
})

const input = ref('')

// The `Dialog` stays **mounted** when closed: without this reset, the input
// of a previous opening would reappear at the next one, as if it had never
// been closed.
watch(
  () => props.open,
  (open) => {
    if (open) input.value = ''
  },
)

function goTo(path: string): void {
  void props.send({ op: 'explore_local', path: path })
}

/**
 * Opens the path typed by hand.
 *
 * The wizard **navigates** to this path instead of declaring it directly,
 * and that is deliberate: it keeps the verification that is the whole point
 * of the dialog — seeing the content and the count of audio files before
 * confirming. A refused path displays right here.
 *
 * It is useful when the target folder is under no offered volume, or when
 * the path is already at hand and it would be absurd to find it again click
 * by click.
 */
function openInput(): void {
  const path = input.value.trim()
  if (!path) return
  goTo(path)
}

/**
 * Returns to picking a volume.
 *
 * Reopening the wizard resets its state on the plugin side: that is exactly
 * what is needed, and it avoids inventing an operation to say so.
 */
function toVolumes(): void {
  void props.send({ op: 'explore_open', kind: 'local' })
}

function descend(name: string): void {
  // The path is composed here: the tree only emits names, because a local
  // path and an SMB path are not composed the same way.
  goTo(`${ex.value.path.replace(/\/$/, '')}/${name}`)
}

function goUp(): void {
  const v = currentVolume.value
  // At the top of a volume, goUp returns to the **volume list** and not to
  // the parent: we do not want to end up browsing `/media` because a USB key
  // was being searched for, nor especially stay locked into the first volume
  // chosen without being able to try another one.
  if (!v || ex.value.path === v.path) {
    toVolumes()
    return
  }
  const parent = ex.value.path.replace(/\/[^/]+\/?$/, '')
  goTo(parent.startsWith(v.path) && parent !== '' ? parent : v.path)
}

async function choose(): Promise<void> {
  const ok = await props.send({
    op: 'add_source',
    kind: 'local',
    path: ex.value.path,
    host: '',
    share: '',
    subpath: null,
    user: '',
    domain: '',
    password: '',
    writable: false,
  })
  if (ok) close()
}

function close(): void {
  void props.send({ op: 'explore_close' })
  emit('close')
}
</script>

<template>
  <Dialog :open="open" @update:open="(v: boolean) => !v && close()">
    <!-- Wider than the kit's default: a folder tree fares poorly in a
         narrow column, and it is already the width the themes dialog
         uses. -->
    <DialogContent class="sm:max-w-2xl" data-device-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_device_title') }}</DialogTitle>
        <!-- Not decorative: reka-ui ties it in via `aria-describedby`, and
             its absence leaves a screen reader announcing a dialog it can
             say nothing about. -->
        <DialogDescription>{{ t('dlg_device_desc') }}</DialogDescription>
      </DialogHeader>

      <!-- `min-w-0` is not decorative: `DialogContent` is a grid, and a grid
           child's minimum width defaults to that of its content. A long
           folder name therefore pushed the grid past the dialog's
           background, and the scrollbar as well as the buttons ended up
           painted outside the white frame. Allowing it to shrink is what
           makes `truncate` work inside it. -->
      <div v-if="!inTree" class="min-w-0 space-y-2">
        <p class="text-sm text-muted-foreground">{{ t('volumes_label') }}</p>
        <!-- An empty list without a sentence would read like a loading that
             never finished. -->
        <p v-if="!data.volumes.length" class="text-sm text-muted-foreground" data-no-volumes>
          {{ t('no_volumes') }}
        </p>
        <button
          v-for="v in data.volumes"
          :key="v.path"
          type="button"
          data-volume
          class="flex w-full min-w-0 items-center gap-2 rounded px-2 py-1 text-left text-sm hover:bg-accent"
          :disabled="frozen"
          @click="goTo(v.path)"
        >
          <span class="min-w-0 flex-1 truncate">{{ v.path }}</span>
          <span class="shrink-0 text-xs text-muted-foreground">{{ v.fstype }}</span>
        </button>
      </div>

      <template v-else>
        <!-- Explicit return to picking the volume: the only other path is
             going up to the top, which is not guessable. -->
        <button
          type="button"
          data-to-volumes
          class="self-start rounded px-2 py-1 text-left text-sm text-muted-foreground hover:bg-accent"
          :disabled="frozen"
          @click="toVolumes"
        >
          ← {{ t('volumes_label') }}
        </button>
        <FolderPicker
          :exploration="ex"
          :t="t"
          :frozen="frozen"
          :path="ex.path"
          @descend="descend"
          @goUp="goUp"
        />
      </template>

      <!-- Direct entry of a path. It **navigates** instead of declaring:
           this keeps the verification that is the whole point of the
           dialog — the folder's content and its audio file count before
           confirming. Useful when the target folder is under no offered
           volume, or when the path is already at hand. -->
      <form class="flex min-w-0 gap-2" @submit.prevent="openInput">
        <Input
          v-model="input"
          data-manual-path
          class="min-w-0 flex-1"
          :placeholder="t('ph_manual_path')"
        />
        <Button
          variant="secondary"
          type="submit"
          data-manual-go
          class="shrink-0"
          :disabled="frozen || !input.trim()"
        >
          {{ t('btn_go') }}
        </Button>
      </form>

      <!-- The refusal displays **here** and not only on the page: behind
           the dialog's grey veil, the page's banner is about invisible at
           the precise moment it matters. -->
      <p v-if="message" class="min-w-0 break-words text-sm text-destructive" data-dlg-message>
        {{ message }}
      </p>

      <div class="flex justify-end gap-2">
        <Button variant="ghost" data-cancel @click="close">{{ t('btn_cancel') }}</Button>
        <Button data-choose :disabled="frozen || !inTree" @click="choose">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
