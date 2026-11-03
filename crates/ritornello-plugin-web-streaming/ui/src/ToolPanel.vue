<script setup lang="ts">
import { Button } from '@ritornello/ui'
import {
  playFailureText, runtimeErrorText, updateErrorText, whenText, type PlaybackFailure, type T, type ToolStatus,
} from './data'

defineProps<{ t: T; tool: ToolStatus; frozen: boolean; lastFailure: PlaybackFailure | null }>()
defineEmits<{ update: [force: boolean]; channel: [nightly: boolean] }>()
</script>

<template>
  <div class="space-y-3 text-sm">
    <p data-tool-version>
      <template v-if="tool.installed && tool.version">{{ t('tool_version', { version: tool.version }) }}</template>
      <template v-else>{{ t('tool_not_installed') }}</template>
    </p>

    <label class="flex cursor-pointer items-start gap-2">
      <input
        type="checkbox"
        class="mt-0.5 size-4 cursor-pointer"
        data-nightly
        :disabled="frozen"
        :checked="tool.channel === 'nightly'"
        @change="$emit('channel', ($event.target as HTMLInputElement).checked)"
      />
      <span>
        {{ t('channel_nightly') }}
        <span class="block text-xs text-muted-foreground">{{ t('channel_nightly_help') }}</span>
      </span>
    </label>

    <div class="flex flex-wrap items-center gap-2">
      <Button
        variant="secondary"
        data-update
        :disabled="frozen || tool.updating"
        @click="$emit('update', false)"
      >
        {{ t('btn_update') }}
      </Button>
      <!-- Only after a refusal a forced update takes (an older release, another
           build of the same number): never for a release the key does not
           vouch for. -->
      <Button
        v-if="tool.forceable"
        variant="outline"
        data-install-anyway
        :disabled="frozen || tool.updating"
        @click="$emit('update', true)"
      >
        {{ t('btn_install_anyway') }}
      </Button>
      <span v-if="tool.updating" class="text-muted-foreground" role="status">{{ t('updating') }}</span>
      <!-- A check that found nothing new changes nothing else on the page:
           this is what says it ran. -->
      <span
        v-else-if="tool.installed && tool.last_check !== null && !tool.error_code"
        class="text-muted-foreground"
        data-checked
      >
        {{ t('checked_at', { when: whenText(tool.last_check) }) }}
      </span>
    </div>

    <p v-if="tool.waiting_for_jobs" class="text-muted-foreground" data-waiting>{{ t('waiting_for_jobs') }}</p>
    <p v-if="tool.error_code" class="text-destructive" role="alert" data-tool-error>
      {{ t('update_failed', { cause: updateErrorText(t, tool.error_code) }) }}
    </p>
    <p v-if="tool.runtime_error_code" class="text-destructive" role="alert" data-runtime-error>
      {{ t('runtime_failed', { cause: runtimeErrorText(t, tool.runtime_error_code) }) }}
    </p>
    <!-- mpv resolves a playback through the wrapper, out of the plugin's
         sight: this is the only place its failure is told at length. -->
    <div v-if="lastFailure" data-last-failure>
      <p>
        {{ t('last_failure', {
          when: whenText(lastFailure.time),
          host: lastFailure.host,
          cause: playFailureText(t, lastFailure.code),
        }) }}
      </p>
      <p v-if="lastFailure.detail" class="break-words text-xs text-muted-foreground" data-last-failure-detail>
        {{ t('last_failure_said', { detail: lastFailure.detail }) }}
      </p>
    </div>
  </div>
</template>
