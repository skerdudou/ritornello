<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { computed } from 'vue'
import { truncateStart, type Exploration, type T } from './data'

/**
 * The picker tree shared by both wizards.
 *
 * Shared, because descending into folders is the same gesture on both
 * sides: only the machine that answers changes. It only emits folder
 * **names**, never paths — composing the path belongs to the caller, a
 * local path and an SMB path not being composed the same way.
 */
const props = defineProps<{
  exploration: Exploration
  t: T
  frozen: boolean
  /**
   * Path **to display**, supplied by the caller.
   *
   * It is no longer derived from `exploration.path`, and that is the fix for
   * a reported defect: on the share side, that field is relative to the
   * share, so the chosen share never appeared anywhere — it seemed to
   * "vanish" upon entering it. Only the caller knows how to compose the full
   * address: `//host/share/...` on one side, absolute path on the other.
   */
  path: string
}>()
defineEmits<{ descend: [name: string]; goUp: [] }>()

/**
 * The path, truncated **from the start** to fit on the line.
 *
 * The tooltip carries the full value: truncating is there to make it fit,
 * not to hide it.
 */
const shortPath = computed(() => truncateStart(props.path))
</script>

<template>
  <!-- `min-w-0` wherever long text runs down, and this is not decorative:
       a grid or flex child's minimum width defaults to that of its content.
       A long path or folder name therefore pushed the dialog past its own
       background, and the scrollbar as well as the buttons ended up
       painted outside the white frame. This is also what makes `truncate`
       work: without permission to shrink, it never gets a chance to cut. -->
  <div class="min-w-0 space-y-2" data-picker>
    <div class="flex min-w-0 items-center gap-2 text-sm">
      <Button
        variant="ghost"
        size="sm"
        class="shrink-0"
        data-picker-go-up
        :disabled="frozen || exploration.busy"
        @click="$emit('goUp')"
      >
        ↑ {{ t('btn_up') }}
      </Button>
      <!-- Truncated **from the start**, by `truncateStart`: on a path, the
           useful information is at the end, and no CSS property knows how
           to cut from that side. The title carries the full path. -->
      <span class="min-w-0 flex-1 truncate text-muted-foreground" data-picker-path :title="path">
        {{ shortPath }}
      </span>
    </div>

    <!-- The refusal replaces the tree: displaying it empty underneath
         would suggest the folder exists and is empty. -->
    <p v-if="exploration.error" class="min-w-0 break-words text-sm text-destructive" data-picker-error>
      {{ exploration.error }}
    </p>

    <template v-else>
      <p v-if="exploration.busy" class="text-sm text-muted-foreground" data-picker-busy>
        {{ t('connecting') }}
      </p>

      <ul class="max-h-64 min-w-0 space-y-1 overflow-y-auto text-sm">
        <li v-for="d in exploration.dirs" :key="d" class="min-w-0">
          <button
            type="button"
            data-picker-folder
            class="block w-full truncate rounded px-2 py-1 text-left hover:bg-accent"
            :disabled="frozen || exploration.busy"
            :title="d"
            @click="$emit('descend', d)"
          >
            📁 {{ d }}
          </button>
        </li>
        <li
          v-if="!exploration.dirs.length && !exploration.busy"
          class="px-2 text-muted-foreground"
          data-picker-empty
        >
          {{ t('empty_folder') }}
        </li>
      </ul>

      <!-- The audio file count of the open level: it is what tells us we
           are in the right place. Without it, a folder is chosen while
           hoping. -->
      <p class="text-sm text-muted-foreground" data-audio-count>
        {{ t('audio_here', { count: exploration.audioCount }) }}
      </p>
    </template>
  </div>
</template>
