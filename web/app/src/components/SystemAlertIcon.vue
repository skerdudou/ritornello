<script setup lang="ts">
import { ExclamationTriangleIcon } from '@radix-icons/vue'
import { useCatalog } from '../composables/useCatalog'
import { useMetrics } from '../composables/useMetrics'

// The warning triangle beside the System entry, in the top nav and in the
// phone's bottom bar alike: the same question ("does the machine need a
// look?"), answered by one component so the two never disagree. What is wrong
// is said on the System page itself; the triangle only sends there.
const { t } = useCatalog()
const { alerts } = useMetrics()
</script>

<template>
  <!-- `role="img"` + `aria-label`: the link's accessible name becomes
       "System, needs attention" — the colour alone would tell a screen reader
       nothing. `title` for the mouse, on a wrapper: an SVG ignores a `title`
       attribute, only a `<title>` child would give it a tooltip. -->
  <span
    v-if="alerts.length > 0"
    data-system-alert-icon
    role="img"
    :aria-label="t('system_alert_badge')"
    :title="t('system_alert_badge')"
    class="inline-flex shrink-0 text-destructive"
  >
    <ExclamationTriangleIcon aria-hidden="true" class="size-4" />
  </span>
</template>
