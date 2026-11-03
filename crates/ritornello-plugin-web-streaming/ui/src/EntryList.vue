<script setup lang="ts">
import { Button, Input } from '@ritornello/ui'
import { ref } from 'vue'
import { videoCount, type Kind, type T } from './data'

/** A preset being edited. `key` is browser-side only (see `WebStreamingAdmin.vue`). */
export interface Row {
  key: number
  /** The number it has on the server, `null` for a row not saved yet. */
  saved: number | null
  url: string
  kind: Kind
  title: string
  channel: string | null
  count: number | null
  site: string | null
  identity: string | null
}

defineProps<{
  t: T
  rows: Row[]
  /** Keys of the rows ticked for download. */
  selected: Set<number>
  /** Some row differs from what the server holds. */
  dirty: boolean
  /** The page could not read the table: it must not overwrite it. */
  frozen: boolean
  /** "Download" is greyed out (no share, nothing ticked, unsaved changes). */
  downloadBlocked: boolean
  noShare: boolean
}>()

const emit = defineEmits<{
  reorder: [from: number, to: number]
  remove: [index: number]
  toggle: [key: number]
  save: []
  download: []
}>()

// Reordering: the preset **is** the position, so moving a row changes its
// number. `dragover.prevent` is essential: without it the browser refuses the
// drop. ▲▼ exist because drag-and-drop is neither keyboard-accessible nor
// reliable with a finger.
const dragging = ref<number | null>(null)

function drop(to: number): void {
  if (dragging.value === null) return
  emit('reorder', dragging.value, to)
  dragging.value = null
}
</script>

<template>
  <div class="space-y-3">
    <p v-if="!rows.length" class="text-sm text-muted-foreground" data-entries-empty>
      {{ t('entries_empty') }}
    </p>
    <table v-else class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="w-8" />
          <th class="w-12 text-left font-normal">{{ t('col_num') }}</th>
          <th class="text-left font-normal">{{ t('col_title') }}</th>
          <th class="w-28" />
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="(r, i) in rows"
          :key="r.key"
          class="border-t border-border"
          :class="dragging === i ? 'opacity-50' : ''"
          draggable="true"
          data-entry-row
          @dragstart="dragging = i"
          @dragover.prevent
          @drop.prevent="drop(i)"
          @dragend="dragging = null"
        >
          <td class="py-1 pl-1">
            <input
              type="checkbox"
              class="size-4 cursor-pointer"
              data-entry-select
              :aria-label="t('select_entry')"
              :checked="selected.has(r.key)"
              @change="emit('toggle', r.key)"
            />
          </td>
          <td class="tabular-nums text-muted-foreground">
            <span class="cursor-grab select-none pr-1" :title="t('reorder_hint')">⠿</span>
            <span data-entry-num>{{ i + 1 }}</span>
          </td>
          <td class="py-1 pr-2">
            <Input v-model="r.title" data-entry-title />
            <!-- textContent only: titles, channels and sites come from remote data. -->
            <span class="text-xs text-muted-foreground" data-entry-detail>
              <template v-if="r.site"><span data-entry-site>{{ r.site }}</span> · </template>
              {{ r.kind === 'playlist' ? t('kind_playlist') : t('kind_video') }}
              <template v-if="r.kind === 'playlist' && r.count !== null"> · {{ videoCount(t, r.count) }}</template>
              <template v-if="r.channel"> · {{ r.channel }}</template>
            </span>
          </td>
          <td class="whitespace-nowrap">
            <Button
              variant="ghost"
              size="icon"
              data-entry-up
              :aria-label="t('move_up')"
              :disabled="i === 0"
              @click="emit('reorder', i, i - 1)"
            >
              ▲
            </Button>
            <Button
              variant="ghost"
              size="icon"
              data-entry-down
              :aria-label="t('move_down')"
              :disabled="i === rows.length - 1"
              @click="emit('reorder', i, i + 1)"
            >
              ▼
            </Button>
            <Button
              variant="ghost"
              size="icon"
              data-entry-delete
              :aria-label="t('remove_entry')"
              @click="emit('remove', i)"
            >
              ✕
            </Button>
          </td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <Button data-save :disabled="frozen" @click="emit('save')">{{ t('btn_save') }}</Button>
      <Button variant="secondary" data-download :disabled="frozen || downloadBlocked" @click="emit('download')">
        {{ t('btn_download') }}
      </Button>
      <span v-if="dirty" class="text-sm text-muted-foreground" data-dirty>
        {{ t('unsaved_changes') }}<template v-if="selected.size"> — {{ t('save_first') }}</template>
      </span>
      <span v-if="noShare" class="text-sm text-muted-foreground" data-no-share>
        {{ t('no_writable_share') }}
      </span>
    </div>
  </div>
</template>
