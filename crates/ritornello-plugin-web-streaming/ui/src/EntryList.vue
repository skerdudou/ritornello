<script setup lang="ts">
import { Button, Input, ReorderHandle, ReorderList } from '@ritornello/ui'
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
// number. The handle grabs a row, finger and mouse alike; ▲▼ are the keyboard
// path.
</script>

<template>
  <div class="space-y-3">
    <p v-if="!rows.length" class="text-sm text-muted-foreground" data-entries-empty>
      {{ t('entries_empty') }}
    </p>
    <table v-else class="w-full text-sm max-md:block">
      <thead class="text-muted-foreground max-md:hidden">
        <tr>
          <th class="w-8" />
          <th class="w-12 text-left font-normal">{{ t('col_num') }}</th>
          <th class="text-left font-normal">{{ t('col_title') }}</th>
          <th class="w-28" />
        </tr>
      </thead>
      <ReorderList
        tag="tbody"
        class="max-md:block"
        :items="rows"
        :item-key="(r) => r.key"
        @move="(from, to) => emit('reorder', from, to)"
        v-slot="{ item: r, index: i, itemAttrs }"
      >
        <tr
          v-bind="itemAttrs"
          class="border-t border-border max-md:flex max-md:flex-wrap max-md:items-center max-md:gap-x-2 max-md:gap-y-1 max-md:py-2 max-md:after:order-2 max-md:after:basis-full max-md:after:content-['']"
          data-entry-row
        >
          <td class="py-1 pl-1 max-md:order-1">
            <input
              type="checkbox"
              class="size-4 cursor-pointer"
              data-entry-select
              :aria-label="t('select_entry')"
              :checked="selected.has(r.key)"
              @change="emit('toggle', r.key)"
            />
          </td>
          <td class="tabular-nums text-muted-foreground max-md:order-1 max-md:mr-auto">
            <ReorderHandle class="pr-1" :title="t('reorder_hint')" />
            <span data-entry-num>{{ i + 1 }}</span>
          </td>
          <td class="py-1 pr-2 max-md:order-3 max-md:basis-full max-md:p-0">
            <Input v-model="r.title" data-entry-title />
            <!-- textContent only: titles, channels and sites come from remote data. -->
            <span class="text-xs text-muted-foreground" data-entry-detail>
              <template v-if="r.site"><span data-entry-site>{{ r.site }}</span> · </template>
              {{ r.kind === 'playlist' ? t('kind_playlist') : t('kind_video') }}
              <template v-if="r.kind === 'playlist' && r.count !== null"> · {{ videoCount(t, r.count) }}</template>
              <template v-if="r.channel"> · {{ r.channel }}</template>
            </span>
          </td>
          <td class="whitespace-nowrap max-md:order-1">
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
      </ReorderList>
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
