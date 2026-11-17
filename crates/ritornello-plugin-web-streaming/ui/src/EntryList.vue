<script setup lang="ts">
import { Button, Input, ReorderHandle, ReorderList } from '@ritornello/ui'
import { ref } from 'vue'
import { videoCount, type Entry, type ProbeLine, type T } from './data'

defineProps<{
  t: T
  /** The server's table, sorted by number. */
  entries: Entry[]
  /** The links being worked through (drawn in the list by a later step). */
  lines: ProbeLine[]
  /** Addresses of the presets ticked for download. */
  selected: Set<string>
  /** The page could not read the table: it must not act on it. */
  frozen: boolean
  /** "Download" is greyed out (no share, nothing ticked). */
  downloadBlocked: boolean
  noShare: boolean
}>()

const emit = defineEmits<{
  remove: [preset: number, url: string]
  move: [preset: number, url: string, to: number]
  rename: [preset: number, url: string, title: string]
  toggle: [url: string]
  choose: [line: number, url: string]
  dismiss: [line: number]
  download: []
  add: []
}>()

// Every action names the preset by its number **and** its address: the plugin
// refuses (`stale`) one that no longer matches its table. Reordering: the
// preset **is** the position, so moving a row changes its number. The handle
// grabs a row, finger and mouse alike; ▲▼ are the keyboard path. `to` is the
// 1-based position the preset ends at.

// The title field holds a draft of its own while it has focus: a read that
// arrives meanwhile (the page polls) must not overwrite what is being typed.
const focused = ref<string | null>(null)
const draft = ref('')

function onFocus(e: Entry): void {
  focused.value = e.url
  draft.value = e.title
}

function onInput(e: Entry, value: string | number): void {
  // Typing implies the focus, whatever the browser reports about it.
  focused.value = e.url
  draft.value = String(value)
}

/** Enter, or leaving the field: a changed, non-blank title is sent; a blank one puts the saved one back. */
function commit(e: Entry): void {
  if (focused.value !== e.url) return
  focused.value = null
  const title = draft.value.trim()
  if (title && title !== e.title) emit('rename', e.preset, e.url, title)
}

/** Escape: the saved title is back, and the field is left. */
function revert(ev: Event): void {
  focused.value = null
  ;(ev.target as HTMLElement).blur()
}

function onEnter(e: Entry, ev: Event): void {
  commit(e)
  ;(ev.target as HTMLElement).blur()
}
</script>

<template>
  <div class="space-y-3">
    <p v-if="!entries.length" class="text-sm text-muted-foreground" data-entries-empty>
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
        :items="entries"
        :item-key="(e) => e.url"
        @move="(from, to) => emit('move', entries[from]!.preset, entries[from]!.url, to + 1)"
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
              :checked="selected.has(r.url)"
              @change="emit('toggle', r.url)"
            />
          </td>
          <td class="tabular-nums text-muted-foreground max-md:order-1 max-md:mr-auto">
            <ReorderHandle class="pr-1" :title="t('reorder_hint')" />
            <span data-entry-num>{{ r.preset }}</span>
          </td>
          <td class="py-1 pr-2 max-md:order-3 max-md:basis-full max-md:p-0">
            <Input
              :model-value="focused === r.url ? draft : r.title"
              data-entry-title
              :disabled="frozen"
              @focus="onFocus(r)"
              @update:model-value="onInput(r, $event)"
              @blur="commit(r)"
              @keydown.enter.prevent="onEnter(r, $event)"
              @keydown.esc.prevent="revert($event)"
            />
            <!-- textContent only: titles, channels and sites come from remote data. -->
            <span class="text-xs text-muted-foreground" data-entry-detail>
              <template v-if="r.site"><span data-entry-site>{{ r.site }}</span> · </template>
              {{ r.kind === 'playlist' ? t('kind_playlist') : t('kind_video') }}
              <template v-if="r.kind === 'playlist' && r.count != null"> · {{ videoCount(t, r.count) }}</template>
              <template v-if="r.channel"> · {{ r.channel }}</template>
            </span>
          </td>
          <td class="whitespace-nowrap max-md:order-1">
            <Button
              variant="ghost"
              size="icon"
              data-entry-up
              :aria-label="t('move_up')"
              :disabled="frozen || i === 0"
              @click="emit('move', r.preset, r.url, i)"
            >
              ▲
            </Button>
            <Button
              variant="ghost"
              size="icon"
              data-entry-down
              :aria-label="t('move_down')"
              :disabled="frozen || i === entries.length - 1"
              @click="emit('move', r.preset, r.url, i + 2)"
            >
              ▼
            </Button>
            <Button
              variant="ghost"
              size="icon"
              data-entry-delete
              :aria-label="t('remove_entry')"
              :disabled="frozen"
              @click="emit('remove', r.preset, r.url)"
            >
              ✕
            </Button>
          </td>
        </tr>
      </ReorderList>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <Button variant="secondary" data-download :disabled="frozen || downloadBlocked" @click="emit('download')">
        {{ t('btn_download') }}
      </Button>
      <span v-if="noShare" class="text-sm text-muted-foreground" data-no-share>
        {{ t('no_writable_share') }}
      </span>
    </div>
  </div>
</template>
