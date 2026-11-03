<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ref } from 'vue'
import { faultText, videoCount, type Found, type ProbeLine, type T } from './data'

defineProps<{
  t: T
  running: boolean
  results: ProbeLine[]
  /** URLs already in the table: adding them again is not offered. */
  present: Set<string>
  frozen: boolean
}>()

const emit = defineEmits<{ probe: [text: string]; add: [found: Found] }>()

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
      <Button data-probe :disabled="frozen || running || !text.trim()" @click="emit('probe', text)">
        {{ t('btn_probe') }}
      </Button>
      <span v-if="running" class="text-sm text-muted-foreground" role="status">{{ t('probing') }}</span>
    </div>

    <ul v-if="results.length" class="space-y-2 text-sm">
      <li v-for="(r, i) in results" :key="i" data-probe-line>
        <!-- The line is what the operator pasted; textContent only. -->
        <p
          v-if="r.state === 'error'"
          class="text-destructive"
          data-probe-error
        >
          <span class="break-all">{{ r.line }}</span> — {{ faultText(t, r) }}
        </p>
        <p v-else-if="r.state === 'pending'" class="text-muted-foreground">
          <span class="break-all">{{ r.line }}</span> …
        </p>
        <div v-else-if="r.choices.length === 1" class="flex items-center gap-2">
          <span class="flex-1">{{ describe(t, r.choices[0]!) }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add
            :disabled="present.has(r.choices[0]!.url)"
            @click="emit('add', r.choices[0]!)"
          >
            {{ present.has(r.choices[0]!.url) ? t('already_added') : t('btn_add') }}
          </Button>
        </div>
        <div v-else class="space-y-1">
          <p class="text-muted-foreground">{{ t('choose_one') }}</p>
          <div v-for="c in r.choices" :key="c.url" class="flex items-center gap-2">
            <span class="flex-1">{{ describe(t, c) }}</span>
            <Button
              variant="secondary"
              size="sm"
              :data-add-choice="c.kind"
              :disabled="present.has(c.url)"
              @click="emit('add', c)"
            >
              {{ present.has(c.url) ? t('already_added') : c.kind === 'playlist' ? t('btn_add_playlist') : t('btn_add_video') }}
            </Button>
          </div>
        </div>
      </li>
    </ul>
  </div>
</template>
