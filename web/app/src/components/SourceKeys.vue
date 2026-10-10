<script setup lang="ts">
import { Button } from '@ritornello/ui'
import SourceIcon from './SourceIcon.vue'
import { useCatalog } from '../composables/useCatalog'

/**
 * One icon key per source, in the Player card's header: the input selector of
 * an amplifier. Where the cycle key needs as many presses as there are
 * notches to the wanted source, a key here takes a single click. `HomeView`
 * draws them only when they fit the header (see `headerMode`); otherwise the
 * cycle key keeps the job.
 *
 * Each key names its source itself (`aria-label`, `title`): `SourceIcon`
 * hides its drawing or initial from assistive technology, so without them a
 * key would be announced as a nameless button.
 *
 * Clicking the key already lit is harmless: awake, the core does nothing on a
 * `SelectSource` naming the active source (`Command::SelectSource`); in
 * standby the same click wakes the device on it.
 */
defineProps<{
  sources: string[]
  icons: Map<string, string>
  active: string | null
  playing: boolean
  disabled: boolean
}>()
const emit = defineEmits<{ choose: [name: string] }>()
const { t } = useCatalog()
</script>

<template>
  <div role="group" :aria-label="t('remote_source')" class="flex items-center gap-1" data-source-keys>
    <Button
      v-for="name in sources"
      :key="name"
      :data-source-key="name"
      :aria-pressed="name === active ? 'true' : 'false'"
      :aria-label="name"
      :title="name"
      :variant="name === active ? 'default' : 'outline'"
      size="icon-sm"
      class="relative"
      :disabled="disabled"
      @click="emit('choose', name)"
    >
      <SourceIcon :name="name" :icon="icons.get(name)" />
      <!-- The dot says "it's playing", as on the source pill it replaces here:
           `bg-current`, so it takes the key's own text colour and contrasts with
           its background by construction (see `PlayerCard.vue`). In the corner,
           clear of the 16-px icon centred in the 32-px key. -->
      <span
        v-if="playing && name === active"
        class="absolute right-0.5 top-0.5 size-1.5 rounded-full bg-current"
        aria-hidden="true"
        data-now-playing-line
      />
    </Button>
  </div>
</template>
