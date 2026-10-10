<script setup lang="ts">
import { computed } from 'vue'
import { isSourceIcon, type SourceIconName } from './sourceIcons'

/**
 * A source's icon, or its initial when it announced none the page knows.
 * Drawn like the Radix icons used elsewhere on the page: 15x15 grid, 1-px
 * strokes in `currentColor`, so the surrounding text colour rules.
 */
const props = withDefaults(defineProps<{ name: string; icon?: string | null; class?: string }>(), {
  icon: null,
  class: 'size-4',
})

const known = computed<SourceIconName | null>(() => (isSourceIcon(props.icon) ? props.icon : null))
const initial = computed(() => props.name.charAt(0).toUpperCase())

// One entry per name of SOURCE_ICONS; each is the inside of the 15x15 svg.
const DRAWINGS: Record<SourceIconName, string> = {
  radio:
    '<rect x="1.5" y="5.5" width="12" height="7.5" rx="1"/><circle cx="9.5" cy="9.25" r="2"/><path d="M3.5 8h2M3.5 10.5h2M3.5 5.5l7-4"/>',
  disc: '<circle cx="7.5" cy="7.5" r="6"/><circle cx="7.5" cy="7.5" r="1.75"/>',
  folder: '<path d="M1.5 3.5h4l1.5 1.5h6.5v7.5h-12z"/>',
  music: '<path d="M5.5 11V3l6-1.5V9.5"/><circle cx="4" cy="11" r="1.5"/><circle cx="10" cy="9.5" r="1.5"/>',
  headphones:
    '<path d="M2 11V7.5a5.5 5.5 0 0 1 11 0V11"/><rect x="1.5" y="9.5" width="3" height="4" rx="1"/><rect x="10.5" y="9.5" width="3" height="4" rx="1"/>',
  podcast:
    '<rect x="5.5" y="1.5" width="4" height="7" rx="2"/><path d="M3 7.5a4.5 4.5 0 0 0 9 0M7.5 12v2M5 14h5"/>',
  tv: '<rect x="1.5" y="3.5" width="12" height="8" rx="1"/><path d="M5 13.5h5M5.5 1.5l2 2 2-2"/>',
  usb: '<path d="M7.5 13.5v-11M7.5 2.5l-1.25 1.75h2.5zM7.5 9L4 7v-1.5M7.5 11l3.5-2V7"/><circle cx="4" cy="5" r="1"/><rect x="10" y="5" width="2" height="2"/>',
}
</script>

<template>
  <svg
    v-if="known"
    :class="props.class"
    viewBox="0 0 15 15"
    fill="none"
    stroke="currentColor"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    :data-source-icon="known"
    v-html="DRAWINGS[known]"
  />
  <span v-else data-source-initial aria-hidden="true" :class="[props.class, 'inline-flex items-center justify-center font-semibold leading-none']">{{ initial }}</span>
</template>
