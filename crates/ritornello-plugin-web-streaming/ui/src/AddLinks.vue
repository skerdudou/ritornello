<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ref } from 'vue'
import { faultText, videoCount, type Found, type ProbeLine, type T } from './data'

defineProps<{
  t: T
  running: boolean
  /** Only the lines that need the operator: two choices, or a failure. */
  lines: ProbeLine[]
  frozen: boolean
}>()

const emit = defineEmits<{ probe: [text: string]; choose: [line: number, url: string] }>()

const text = ref('')

function describe(t: T, f: Found): string {
  const kind = f.kind === 'playlist' ? t('kind_playlist') : t('kind_video')
  const count = f.kind === 'playlist' && f.count !== null ? ` · ${videoCount(t, f.count)}` : ''
  const site = f.site ? `${f.site} · ` : ''
  return `${f.title} (${site}${kind}${count})`
}
</script>

<template>
  <div class="space-y-3">
    <textarea
      v-model="text"
      data-links
      rows="4"
      class="border-input bg-transparent placeholder:text-muted-foreground w-full min-w-0 rounded-md border px-3 py-2 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-3"
      :placeholder="t('add_placeholder')"
    />
    <div class="flex flex-wrap items-center gap-2">
      <Button data-probe :disabled="frozen || !text.trim()" @click="emit('probe', text)">
        {{ t('btn_probe') }}
      </Button>
      <span v-if="running" class="text-sm text-muted-foreground" role="status">{{ t('probing') }}</span>
    </div>

    <ul v-if="lines.some((l) => l.state === 'found' || l.state === 'error')" class="space-y-2 text-sm">
      <template v-for="r in lines" :key="r.id">
        <li v-if="r.state === 'error'" data-probe-line>
          <!-- The line is what the operator pasted; textContent only. -->
          <p class="text-destructive" data-probe-error>
            <span class="break-all">{{ r.line }}</span> — {{ faultText(t, r) }}
          </p>
        </li>
        <li v-else-if="r.state === 'found'" data-probe-line class="space-y-1">
          <p class="text-muted-foreground">{{ t('choose_one') }}</p>
          <div v-for="c in r.choices" :key="c.url" class="flex flex-wrap items-center gap-2">
            <span class="min-w-0 flex-1 wrap-anywhere">{{ describe(t, c) }}</span>
            <Button
              variant="secondary"
              size="sm"
              :data-add-choice="c.kind"
              :disabled="c.present"
              @click="emit('choose', r.id, c.url)"
            >
              {{ c.present ? t('already_added') : c.kind === 'playlist' ? t('btn_add_playlist') : t('btn_add_video') }}
            </Button>
          </div>
        </li>
      </template>
    </ul>
  </div>
</template>
