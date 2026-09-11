<script setup lang="ts">
import { computed, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { UpdatePayload } from '../types'

/**
 * What the device has to say about itself in one line, plus the detail behind
 * a fold.
 *
 * Split out of `UpdateCard.vue` rather than grown inside it: the card is
 * about to also hold the automatic policy and the beta channel, and a card
 * that renders a payload **and** owns a save path needs its reading half to
 * be testable on its own.
 *
 * The line counts every component out of step, the core included. It used to
 * name the core alone, which is what made an owner read "up to date" while
 * ten plugins were being offered.
 */
const props = defineProps<{ update: UpdatePayload }>()
const { t } = useCatalog()
const open = ref(false)

/** Out of step, in payload order. The core comes first because the core's row
 *  is first in the payload, not because this sorts. */
const outOfStep = computed(() =>
  props.update.components.filter((c) => c.availability === 'update_available'),
)

/**
 * The outcomes that count nothing keep their own sentence.
 *
 * Each of these rebuilt every row against an empty offer, so every row reads
 * `unknown`: a count would then be a statement about a device that has not
 * looked, and "Up to date" would be a lie. `installed` is deliberately absent
 * — it is a transient report shown alongside the summary, not instead of it.
 */
const outcomeSentence = computed(() => {
  switch (props.update.outcome.kind) {
    case 'no_release':
      return t.value('update_no_release')
    case 'only_prereleases':
      return t.value('update_only_prereleases')
    case 'never_checked':
      return t.value('update_never_checked')
    default:
      return null
  }
})

const summary = computed(() => {
  if (outcomeSentence.value) return outcomeSentence.value
  const rows = outOfStep.value
  if (rows.length === 0) return t.value('update_aligned')
  if (rows.length === 1) {
    const only = rows[0]!
    return t.value('update_out_of_step_one', {
      name: only.name,
      installed: only.installed ?? '?',
      offered: only.offered ?? '?',
    })
  }
  return t.value('update_out_of_step', {
    count: rows.length,
    version: props.update.release_version ?? '?',
  })
})
</script>

<template>
  <div class="space-y-2">
    <p data-update-summary class="text-sm font-medium">{{ summary }}</p>
    <div v-if="outOfStep.length > 1">
      <button
        type="button"
        data-update-detail-toggle
        class="text-xs text-muted-foreground underline"
        @click="open = !open"
      >
        {{ t('update_detail') }}
      </button>
      <ul v-if="open" class="mt-1 space-y-0.5">
        <li
          v-for="c in outOfStep"
          :key="c.name"
          data-update-detail-row
          class="text-xs text-muted-foreground"
        >
          {{ c.name }} — {{ c.installed ?? '?' }} → {{ c.offered ?? '?' }}
        </li>
      </ul>
    </div>
  </div>
</template>
