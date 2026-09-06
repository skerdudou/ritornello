<script setup lang="ts">
import { computed } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { useMetrics } from '../composables/useMetrics'

/**
 * Does the device answer? One badge in the header, on every page.
 *
 * It invents no probe of its own: `useMetrics` already interrogates
 * `/api/system` from the moment the SPA mounts until the tab closes, for the
 * whole app. This component only renders the answer — and the state machine
 * itself stays in the composable, where the facts are, rather than being
 * re-derived here (see `connection`).
 */
const { t } = useCatalog()
const { connection } = useMetrics()

const KEY = {
  online: 'connection_online',
  offline: 'connection_offline',
  unknown: 'connection_unknown',
} as const

/**
 * The dot is the fast signal, read across a living room; the word next to it
 * is what makes the badge readable by someone who does not distinguish the
 * two colours, and by a screen reader. So the colour never travels alone.
 */
const DOT = {
  online: 'bg-success',
  offline: 'bg-destructive',
  unknown: 'bg-muted-foreground',
} as const

const label = computed(() => t.value(KEY[connection.value]))
</script>

<template>
  <!-- `aria-live="polite"` and not a silent badge: the whole point is the
       transition. Someone reading a plugin page must be told the device
       dropped, rather than having to notice a colour change in a corner. -->
  <span
    role="status"
    aria-live="polite"
    :data-connection="connection"
    class="flex items-center gap-1.5 text-sm text-muted-foreground"
  >
    <span aria-hidden="true" :class="['size-2 shrink-0 rounded-full', DOT[connection]]" />
    <!-- `sr-only sm:not-sr-only`, never `hidden`: on a phone the header only
         has room for the brand, the theme toggle and this dot, so the word is
         dropped **visually**. `hidden` would also take it out of the
         accessibility tree and leave a screen reader with a coloured dot and
         nothing to read. Being absolutely positioned, the hidden label takes
         no part in the flex gap either. -->
    <span data-connection-label class="sr-only sm:not-sr-only">{{ label }}</span>
  </span>
</template>
