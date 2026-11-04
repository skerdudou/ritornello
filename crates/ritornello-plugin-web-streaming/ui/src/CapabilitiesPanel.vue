<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { computed } from 'vue'
import type { Capabilities, T } from './data'

const props = defineProps<{ t: T; caps: Capabilities; frozen: boolean }>()
defineEmits<{ recheck: [] }>()

/** How each runtime is usually written; the wire carries yt-dlp's names. */
const RUNTIME_NAMES: Record<string, string> = { deno: 'Deno', node: 'Node.js', quickjs: 'QuickJS' }

/**
 * The JavaScript line, built as one string. Two `<template>` blocks side by
 * side lost the space between them (the compiler drops whitespace that opens
 * a block), which printed "quickjsQuickJS est…".
 */
const jsLine = computed(() => {
  const { t, caps } = props
  if (!caps.js_runtime) return caps.js_hopeless ? t('js_hopeless') : t('js_none')
  const found = t('js_found', { name: RUNTIME_NAMES[caps.js_runtime] ?? caps.js_runtime })
  return caps.js_runtime_slow ? `${found} ${t('js_slow')}` : found
})
</script>

<template>
  <div class="space-y-2 text-sm">
    <p data-python>
      <template v-if="caps.python3">{{ t('python_found', { version: caps.python3 }) }}</template>
      <!-- The exact command, when the plugin has one. -->
      <template v-else>
        {{ t('python_missing', { command: caps.apt_hint ?? '' }) }}
      </template>
    </p>

    <!-- No command here: nothing the operator could type would help. -->
    <p data-js>{{ jsLine }}</p>

    <div class="flex items-center gap-2">
      <Button variant="outline" size="sm" data-recheck :disabled="frozen || caps.checking" @click="$emit('recheck')">
        {{ t('btn_recheck') }}
      </Button>
      <span v-if="caps.checking" class="text-muted-foreground" role="status">{{ t('checking') }}</span>
    </div>
  </div>
</template>
