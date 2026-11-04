<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { faultText, noteText, rowTitle, stateText, targetText, type Job, type T } from './data'

defineProps<{ t: T; jobs: Job[] }>()
defineEmits<{ cancel: [id: number] }>()

/** Newest first: what the operator just asked for is on top. */
function newestFirst(jobs: Job[]): Job[] {
  return [...jobs].sort((a, b) => b.id - a.id)
}
</script>

<template>
  <p v-if="!jobs.length" class="text-sm text-muted-foreground">{{ t('jobs_empty') }}</p>
  <ul v-else class="space-y-4 text-sm">
    <li v-for="j in newestFirst(jobs)" :key="j.id" class="space-y-1" data-job>
      <div class="flex flex-wrap items-center gap-2">
        <span class="font-medium">{{ t('job_line', { id: j.id, target: targetText(j.target) }) }}</span>
        <span class="text-muted-foreground" data-job-state>{{ stateText(t, j.state) }}</span>
        <span v-if="j.detail" class="text-destructive">{{ faultText(t, j.detail) }}</span>
        <Button
          v-if="j.state === 'queued' || j.state === 'running'"
          variant="outline"
          size="sm"
          data-job-cancel
          @click="$emit('cancel', j.id)"
        >
          {{ t('btn_cancel') }}
        </Button>
      </div>
      <ul v-if="j.rows.length" class="space-y-0.5 pl-4">
        <li v-for="(r, i) in j.rows" :key="i" class="flex flex-wrap gap-x-2" data-job-row>
          <span class="min-w-0 flex-1 truncate" data-job-row-title>{{ rowTitle(t, j.rows, i) }}</span>
          <span class="text-muted-foreground">
            {{ stateText(t, r.state) }}<template v-if="r.state === 'running' && r.percent !== undefined"> {{ Math.round(r.percent) }}%</template>
          </span>
          <span v-if="r.detail" :class="r.state === 'failed' ? 'text-destructive' : 'text-muted-foreground'">
            {{ faultText(t, r.detail) }}
          </span>
          <span v-for="(n, k) in r.notes ?? []" :key="k" class="w-full pl-2 text-muted-foreground" data-job-row-note>
            {{ noteText(t, n) }}
          </span>
        </li>
      </ul>
    </li>
  </ul>
</template>
