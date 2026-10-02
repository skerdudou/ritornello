<script setup lang="ts">
/**
 * What the check a dialog runs on opening looks like, shared by "Add a
 * plugin" and "Add a language" so the two say the same thing the same way:
 * a spinner while it runs, "busy, try again" for a full queue, and the
 * failure with a Retry button. Pure render: `useUpdateCheck` owns the state.
 */
import { Button } from '@ritornello/ui'
import { ReloadIcon } from '@radix-icons/vue'
import { useCatalog } from '../composables/useCatalog'
import type { CheckPhase } from '../composables/useUpdateCheck'

defineProps<{
  phase: CheckPhase
  /** The failure to show, when the last check failed; `null` otherwise. */
  failure: string | null
}>()
const emit = defineEmits<{ retry: [] }>()
const { t } = useCatalog()
</script>

<template>
  <p
    v-if="phase === 'checking'"
    data-check-running
    class="flex items-center gap-2 text-sm text-muted-foreground"
  >
    <ReloadIcon class="size-4 animate-spin" aria-hidden="true" />
    {{ t('installables_checking') }}
  </p>
  <div
    v-else-if="phase === 'timeout'"
    data-check-slow
    class="flex flex-wrap items-center gap-2 text-sm text-muted-foreground"
  >
    <ReloadIcon class="size-4 animate-spin" aria-hidden="true" />
    <span>{{ t('installables_slow') }}</span>
    <Button variant="outline" size="xs" data-check-retry @click="emit('retry')">{{ t('installables_retry') }}</Button>
  </div>
  <div
    v-else-if="phase === 'queue_full'"
    data-check-queue-full
    class="flex flex-wrap items-center gap-2 text-sm text-muted-foreground"
  >
    <span>{{ t('installables_queue_busy') }}</span>
    <Button variant="outline" size="xs" data-check-retry @click="emit('retry')">{{ t('installables_retry') }}</Button>
  </div>
  <div
    v-else-if="failure !== null"
    data-check-failed
    class="flex flex-wrap items-center gap-2 text-sm text-destructive"
  >
    <span>{{ t('update_last_attempt_failed') }}: {{ failure }}</span>
    <Button variant="outline" size="xs" data-check-retry @click="emit('retry')">{{ t('installables_retry') }}</Button>
  </div>
</template>
