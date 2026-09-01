<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ref } from 'vue'
import DeviceDialog from './DeviceDialog.vue'
import ShareDialog from './ShareDialog.vue'
import { rootTarget, type Data, type Send, type T } from './data'

/**
 * The declared sources.
 *
 * No more input form: an address is no longer typed blindly, it is browsed
 * then declared. This pane is therefore reduced to enumerating what exists,
 * offering the three gestures that apply to a source, and opening one of
 * the two wizards.
 */
const props = defineProps<{
  data: Data
  t: T
  send: Send
  frozen: boolean
  /** Last refusal from the plugin, relayed to the dialogs so it displays there. */
  message: string
}>()

const deviceOpen = ref(false)
const shareOpen = ref(false)

function open(kind: 'local' | 'smb'): void {
  // Opening goes through the plugin, not only through a local boolean: it
  // is the plugin that carries the wizard's state (browsed volumes, open
  // session), and a dialog displaying without notifying it would inherit
  // the previous one's state.
  void props.send({ op: 'explore_open', kind })
  if (kind === 'local') deviceOpen.value = true
  else shareOpen.value = true
}

function addAll(name: string): void {
  // Empty path = the whole source. Recursive and asynchronous on the plugin
  // side: the response does not wait for the scan to finish, it is the
  // page's polling that shows its progress.
  void props.send({ op: 'add_dir', root: name, path: '' })
}

function remove(name: string): void {
  void props.send({ op: 'remove_source', name })
}

function toggle(name: string, writable: boolean): void {
  void props.send({ op: 'set_writable', name, writable })
}

function goUp(): void {
  void props.send({ op: 'mount' })
}
</script>

<template>
  <!-- No title, like the other two panes: the tab already carries it, and
       `TabsContent` makes it this section's accessible name. -->
  <section class="space-y-4" data-sources-pane>
    <p v-if="!data.roots.length" class="text-sm text-muted-foreground" data-no-sources>
      {{ t('no_sources') }}
    </p>

    <div
      v-for="r in data.roots"
      :key="r.name"
      data-source-row
      class="flex flex-wrap items-center gap-2 rounded-md border border-border p-3"
    >
      <span class="text-xs text-muted-foreground" data-source-kind>
        {{ r.kind === 'local' ? t('kind_local') : t('kind_smb') }}
      </span>
      <span class="min-w-48 flex-1 truncate text-sm" data-source-target>{{ rootTarget(r) }}</span>

      <!-- The mount state is **observed**, never entered: it comes from
           the plugin, which reads /proc/mounts. -->
      <span v-if="r.kind === 'smb'" class="text-xs" data-source-mounted>
        {{ r.mounted ? t('mounted_yes') : t('mounted_no') }}
      </span>

      <!-- The retry follows the **observed** state, not the memory of the
           last attempt: `mount_error` lives in the plugin's memory and
           resets empty at every restart, so "not mounted" was found again
           with nothing left to fix it. `mounted`, on the other hand, is
           read from /proc/mounts and always tells the truth.
           And it lives on the source's row, right where the problem shows. -->
      <Button
        v-if="r.kind === 'smb' && !r.mounted"
        variant="outline"
        size="sm"
        data-retry-mount
        :disabled="frozen"
        @click="goUp"
      >
        {{ t('btn_retry_mount') }}
      </Button>

      <label v-if="r.kind === 'smb'" class="flex items-center gap-1 text-sm">
        <input
          type="checkbox"
          data-writable
          :checked="r.writable"
          :disabled="frozen"
          @change="toggle(r.name, ($event.target as HTMLInputElement).checked)"
        />
        {{ t('writable_label') }}
      </label>

      <Button
        variant="secondary"
        size="sm"
        data-add-all
        :disabled="frozen"
        @click="addAll(r.name)"
      >
        {{ t('btn_add_to_playlist') }}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        data-remove-source
        :aria-label="t('btn_remove_source')"
        :disabled="frozen"
        @click="remove(r.name)"
      >
        ✕
      </Button>
    </div>

    <!-- Mounting follows the declaration, with no button for the user to
         find. Without this report, a source would stay "not mounted"
         without ever saying why. Text alone: the retry lives on the row
         concerned, and two buttons for the same gesture would cause
         hesitation. -->
    <p
      v-if="data.mountError"
      class="min-w-0 break-words text-sm text-destructive"
      data-mount-error
    >
      {{ t('mount_error_title') }} {{ data.mountError }}
    </p>

    <!-- A mount that no longer responds is **not** a mount failure: it is
         mounted, it has gone silent. Two distinct causes, hence two
         distinct messages — confusing them would send the user retrying a
         mount that succeeded. This is also what explains the "unclear"
         states of the queue and the durations that do not arrive; without
         this block, the user has no cause to link them to. -->
    <div
      v-if="data.unresponsive.length"
      class="min-w-0 rounded border border-destructive/50 p-2 text-sm"
      data-unresponsive
    >
      <p class="text-destructive">{{ t('unresponsive_title') }}</p>
      <ul class="ml-4 list-disc break-all font-mono text-xs">
        <li v-for="m in data.unresponsive" :key="m">{{ m }}</li>
      </ul>
      <p class="mt-1 text-muted-foreground">{{ t('unresponsive_hint') }}</p>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <Button variant="secondary" data-add-device :disabled="frozen" @click="open('local')">
        {{ t('btn_add_device') }}
      </Button>
      <Button variant="secondary" data-add-share :disabled="frozen" @click="open('smb')">
        {{ t('btn_add_share') }}
      </Button>
    </div>

    <DeviceDialog
      :data="data"
      :t="t"
      :send="send"
      :frozen="frozen"
      :message="message"
      :open="deviceOpen"
      @close="deviceOpen = false"
    />
    <ShareDialog
      :data="data"
      :t="t"
      :send="send"
      :frozen="frozen"
      :message="message"
      :open="shareOpen"
      @close="shareOpen = false"
    />
  </section>
</template>
