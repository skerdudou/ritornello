<script setup lang="ts">
import type { HTMLAttributes } from "vue"
import { cn } from "@/lib/utils"
import { Button } from "../button"

/**
 * The one `(?)` of the whole UI.
 *
 * It exists because the affordance had been rebuilt at each call site and had
 * drifted three ways: a drawn question mark in a circle on the listening page,
 * and a bare `?` text glyph in a square on the System tab -- twice, aligned
 * differently from one another. A bare `?` also reads as stray punctuation
 * where a circled glyph reads as a button, so the drawn one won.
 *
 * What it therefore fixes for good: the glyph, the ghost variant, the round
 * shape, the muted colour, the vertical alignment, and the `title` mirrored
 * from the accessible name -- a native tooltip the two System buttons did not
 * have.
 *
 * `align-middle` is not decoration: `Button` is `inline-flex`, so dropped into
 * a line of text it aligns on the baseline by default, which puts a 24 px box
 * *above* the line it belongs to. That is exactly how the under-voltage one
 * looked wrong.
 */
const props = withDefaults(defineProps<{
  /**
   * Accessible name, also used as the native `title`. Both, not one: the
   * tooltip serves the mouse, the label serves the screen reader, and a `(?)`
   * with no text of its own has nothing else to announce.
   */
  label: string
  /**
   * Context, not pixels. `inline` sits next to a label or a card title;
   * `touch` is the 44 px recommended tap target, for a `(?)` that shares its
   * line with something overflowing into it (see `ProvenanceDetails.vue`).
   * Naming the context rather than a size keeps the two dimensions -- button
   * box and glyph -- decided here rather than at each call site.
   */
  size?: "inline" | "touch"
  class?: HTMLAttributes["class"]
}>(), { size: "inline" })

const SIZES = {
  inline: "size-6 [&_svg]:size-4",
  touch: "size-11 [&_svg]:size-[18px]",
} as const
</script>

<template>
  <Button
    variant="ghost"
    :aria-label="label"
    :title="label"
    :class="cn('shrink-0 rounded-full align-middle text-muted-foreground', SIZES[props.size], props.class)"
  >
    <!-- Drawn rather than pulled from an icon set: this is the shape the
         listening page already carried, and it is the one the owner picked.
         `aria-hidden`: the accessible name is on the button. -->
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="9" />
      <path d="M9.6 9.2a2.5 2.5 0 1 1 3.2 3.4c-.5.3-.8.8-.8 1.4v.4" />
      <path d="M12 17.4h.01" />
    </svg>
  </Button>
</template>
