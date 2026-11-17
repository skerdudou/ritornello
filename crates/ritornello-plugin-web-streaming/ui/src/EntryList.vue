<script setup lang="ts">
import { Button, Input, ReorderHandle, ReorderList } from '@ritornello/ui'
import { ref, watch } from 'vue'
import { faultText, videoCount, type Entry, type ProbeLine, type T } from './data'

const props = defineProps<{
  t: T
  /** The server's table, sorted by number. */
  entries: Entry[]
  /** The links being worked through, drawn after the presets. */
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

// A title just sent stays on screen until the page's re-read has landed (the
// table is replaced then, whatever the answer): without it the old title
// flashes back between Enter and the read. A refused rename shows the saved
// title again, because the re-read brings it back.
const sent = ref<{ url: string; title: string } | null>(null)
watch(
  () => props.entries,
  () => {
    sent.value = null
  },
)

function shown(e: Entry): string {
  if (focused.value === e.url) return draft.value
  return sent.value?.url === e.url ? sent.value.title : e.title
}

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
  if (title && title !== e.title) {
    sent.value = { url: e.url, title }
    emit('rename', e.preset, e.url, title)
  }
}

/** Escape: the saved title is back, and the field is left. */
function revert(ev: Event): void {
  focused.value = null
  ;(ev.target as HTMLElement).blur()
}

function onEnter(e: Entry, ev: KeyboardEvent): void {
  // The Enter that validates an IME composition is not the operator's Enter.
  if (ev.isComposing) return
  ev.preventDefault()
  commit(e)
  ;(ev.target as HTMLElement).blur()
}
</script>

<template>
  <div class="space-y-3">
    <p v-if="!entries.length && !lines.length" class="text-sm text-muted-foreground" data-entries-empty>
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
              :model-value="shown(r)"
              data-entry-title
              :disabled="frozen"
              @focus="onFocus(r)"
              @update:model-value="onInput(r, $event)"
              @blur="commit(r)"
              @keydown.enter="onEnter(r, $event)"
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
      <!-- The links being worked through: no number, no checkbox. Everything
           that comes from a link or from the tool is text only. -->
      <tbody v-if="lines.length" class="max-md:block" data-lines>
        <tr
          v-for="l in lines"
          :key="l.id"
          class="border-t border-border max-md:flex max-md:flex-wrap max-md:items-center max-md:gap-2 max-md:py-2"
          data-probe-line
          :data-line-state="l.state"
        >
          <td colspan="3" class="py-2 pl-1 pr-2 max-md:flex-1 max-md:p-0">
            <p v-if="l.state === 'error'" class="text-destructive" data-probe-error>
              <span class="break-all">{{ l.line }}</span> — {{ faultText(t, l) }}
            </p>
            <template v-else>
              <p class="break-all text-muted-foreground" data-line-text>{{ l.line }}</p>
              <p v-if="l.state === 'pending'" class="text-xs text-muted-foreground" data-line-waiting>
                {{ t('line_waiting') }}
              </p>
              <p v-else-if="l.state === 'probing'" class="text-xs text-muted-foreground" role="status" data-line-probing>
                <span class="inline-block animate-spin" aria-hidden="true">↻</span>
                {{ t('line_probing') }}
              </p>
              <div v-else-if="l.state === 'found'" class="mt-1 space-y-1">
                <div v-for="c in l.choices" :key="c.url" class="flex flex-wrap items-center gap-2">
                  <span class="min-w-0 flex-1 wrap-anywhere">{{ c.title }}</span>
                  <Button
                    variant="secondary"
                    size="sm"
                    :data-add-choice="c.kind"
                    :disabled="frozen || c.present"
                    @click="emit('choose', l.id, c.url)"
                  >
                    {{ c.present ? t('fault_already_present') : c.kind === 'playlist' ? t('keep_playlist') : t('keep_video') }}
                  </Button>
                </div>
              </div>
            </template>
          </td>
          <td class="whitespace-nowrap text-right max-md:order-1">
            <Button
              variant="ghost"
              size="icon"
              data-line-dismiss
              :aria-label="t('dismiss_line')"
              :disabled="frozen"
              @click="emit('dismiss', l.id)"
            >
              ✕
            </Button>
          </td>
        </tr>
      </tbody>
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
