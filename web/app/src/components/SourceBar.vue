<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { useCatalog } from '../composables/useCatalog'

/**
 * One key per source, the input selector of an amplifier: where the cycle key
 * needs as many presses as there are notches to the wanted source, this one
 * takes a single click.
 *
 * Only drawn from `lg` up, where the page has the width for it; below, the
 * cycle key in the corner of the Player card keeps the job (see `HomeView`,
 * which decides which of the two shows). The name shown is the source's
 * own, exactly as the Player card's pill shows it, so both say the same word.
 *
 * Clicking the key already lit is harmless: the core does nothing on a
 * `SelectSource` naming the active source (`Command::SelectSource`).
 */
defineProps<{
  sources: string[]
  active: string | null
  disabled: boolean
}>()
const emit = defineEmits<{ choose: [name: string] }>()
const { t } = useCatalog()
</script>

<template>
  <div data-source-bar role="group" :aria-label="t('remote_source')" class="hidden auto-cols-fr grid-flow-col gap-2 lg:grid">
    <Button
      v-for="name in sources"
      :key="name"
      :data-source-key="name"
      :aria-pressed="name === active ? 'true' : 'false'"
      :variant="name === active ? 'default' : 'outline'"
      class="h-11"
      :disabled="disabled"
      @click="emit('choose', name)"
    >
      {{ name }}
    </Button>
  </div>
</template>
