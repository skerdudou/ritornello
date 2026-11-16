<script setup lang="ts">
import { computed, inject, ref } from "vue"
import { REORDER_DISABLED } from "./context"

/**
 * The ⠿ grip a row is dragged by -- the only place a drag can start.
 *
 * `aria-hidden`: dragging is a pointer gesture with nothing to announce; a
 * screen reader would only read a braille glyph.
 *
 * `touch-none` is what lets a finger drag at all: without it the browser
 * claims the gesture to scroll the page and cancels the pointer. Only on the
 * handle, so the rest of the row still scrolls. `select-none` and
 * `-webkit-touch-callout:none` keep a long press from selecting the glyph or
 * opening iOS's callout instead of grabbing the row.
 *
 * The doc lives here and not above the template on purpose: an HTML comment
 * before the root element gives the component two root nodes, and the
 * fallthrough `title` would land nowhere.
 */
const disabled = inject(REORDER_DISABLED, ref(false))

const classes = computed(() => [
  "inline-flex h-9 w-7 items-center justify-center select-none touch-none [-webkit-touch-callout:none] text-muted-foreground",
  disabled.value ? "cursor-not-allowed opacity-40" : "cursor-grab active:cursor-grabbing",
])
</script>

<template>
  <span data-reorder-handle aria-hidden="true" :class="classes">⠿</span>
</template>
