<script lang="ts" setup>
import type { ToasterProps } from "vue-sonner"
import { CheckCircledIcon, InfoCircledIcon, ReloadIcon, CrossCircledIcon, ExclamationTriangleIcon, Cross2Icon } from '@radix-icons/vue'
import { Toaster as Sonner } from "vue-sonner"
import { cn } from "@/lib/utils"
// The vue-sonner stylesheet is not imported here but in `src/theme.css`: this
// component goes into `ui-kit.js`, which the shell **externalizes** (see
// `rollupOptions.external` in `web/app/vite.config.ts`), so a CSS import placed
// here would never reach the served sheet. The variables set below only dress
// a positioning that this sheet alone provides.

const props = defineProps<ToasterProps>()
</script>

<template>
  <Sonner
    :class="cn('toaster group', props.class)"
    :style="{
      '--normal-bg': 'var(--popover)',
      '--normal-text': 'var(--popover-foreground)',
      '--normal-border': 'var(--border)',
      '--border-radius': 'var(--radius)',
    }"
    v-bind="props"
  >
    <template #success-icon>
      <CheckCircledIcon class="size-4" />
    </template>
    <template #info-icon>
      <InfoCircledIcon class="size-4" />
    </template>
    <template #warning-icon>
      <ExclamationTriangleIcon class="size-4" />
    </template>
    <template #error-icon>
      <CrossCircledIcon class="size-4" />
    </template>
    <template #loading-icon>
      <div>
        <ReloadIcon class="size-4 animate-spin" />
      </div>
    </template>
    <template #close-icon>
      <Cross2Icon class="size-4" />
    </template>
  </Sonner>
</template>
