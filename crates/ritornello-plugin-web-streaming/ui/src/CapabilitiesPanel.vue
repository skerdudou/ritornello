<script setup lang="ts">
import { Button } from '@ritornello/ui'
import type { Capabilities, T } from './data'

defineProps<{ t: T; caps: Capabilities; frozen: boolean }>()
defineEmits<{ recheck: [] }>()
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
    <p data-js>
      <template v-if="caps.js_runtime">{{ t('js_found', { name: caps.js_runtime }) }}</template>
      <template v-else-if="caps.js_hopeless">{{ t('js_hopeless') }}</template>
      <template v-else>{{ t('js_none') }}</template>
      <template v-if="caps.js_runtime && caps.js_runtime_slow"> {{ t('js_slow') }}</template>
    </p>

    <div class="flex items-center gap-2">
      <Button variant="outline" size="sm" data-recheck :disabled="frozen || caps.checking" @click="$emit('recheck')">
        {{ t('btn_recheck') }}
      </Button>
      <span v-if="caps.checking" class="text-muted-foreground" role="status">{{ t('checking') }}</span>
    </div>
  </div>
</template>
