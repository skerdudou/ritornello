<script setup lang="ts">
import type { Component } from 'vue'

// Below `md` a button shows only `icon`; `label` stays in the DOM
// (`max-md:sr-only`) so the word is still there for the layout tests and
// screen readers. The icon sits in a <span> rather than directly in the
// button so the kit's `has-[>svg]` padding never applies on a computer.
defineProps<{ icon: Component; label: string }>()
</script>

<template>
  <span class="md:hidden" aria-hidden="true" data-phone-icon><component :is="icon" class="size-4" /></span>
  <span class="max-md:sr-only" data-label>{{ label }}</span>
</template>
