<script setup lang="ts">
import type { HTMLAttributes } from "vue"
import { cn } from "@/lib/utils"

/**
 * A placeholder block standing in for content that has not arrived yet.
 *
 * `aria-hidden`: a placeholder carries no information. The wait itself is
 * announced once, by the container that owns the `role="status"` — otherwise a
 * screen reader would read a run of empty boxes.
 *
 * `motion-safe:` and not a plain `animate-pulse`: the variant compiles to a
 * `prefers-reduced-motion: no-preference` query, so a reader who asked the
 * system for less movement gets a still block rather than a breathing one. The
 * placeholder stays visible either way; only the pulse is optional.
 *
 * The doc lives here and not above the template on purpose: an HTML comment
 * before the root element gives the component **two** root nodes, so Vue
 * renders a fragment — attributes then land nowhere and `aria-hidden` silently
 * disappears.
 */
const props = defineProps<{ class?: HTMLAttributes["class"] }>()
</script>

<template>
  <div
    aria-hidden="true"
    data-slot="skeleton"
    :class="cn('motion-safe:animate-pulse rounded-md bg-muted', props.class)"
  />
</template>
