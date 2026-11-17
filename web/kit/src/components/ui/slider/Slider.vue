<script setup lang="ts">
import type { SliderRootEmits, SliderRootProps } from "reka-ui"
import type { HTMLAttributes } from "vue"
import { reactiveOmit } from "@vueuse/core"
import { SliderRange, SliderRoot, SliderThumb, SliderTrack, useForwardPropsEmits } from "reka-ui"
import { computed, useAttrs } from "vue"
import { cn } from "@/lib/utils"

defineOptions({
  inheritAttrs: false,
})

// A single thumb, always: the project's usages (progress, volume, equalizer
// bands) are scalar values. `py-[19px]`: 19 + 6 + 19 = 44 px of touch area
// around a 6 px track — the minimum finger target, carried by the padding and
// not by the track, which keeps its thinness. `orientation="vertical"` (the
// equalizer) turns the same 44 px sideways: the `data-[orientation=vertical]`
// variants below, set by reka-ui on the root, the track and the range, so a
// horizontal slider renders exactly as before. A vertical one takes its
// height from the caller's class (`h-32`, ...).
const props = defineProps<SliderRootProps & { class?: HTMLAttributes["class"] }>()
const emits = defineEmits<SliderRootEmits>()
const delegatedProps = reactiveOmit(props, "class")
const forwarded = useForwardPropsEmits(delegatedProps, emits)

// `aria-label` is not a declared prop of `SliderRootProps`: without this
// sorting, it falls into the component's `$attrs` and `v-bind="forwarded"`
// lets it leak to `SliderRoot`, which sets it on its enclosing `<span>` — not
// on the thumb. Yet `SliderThumbImpl` computes ITS `aria-label` from the attrs
// passed to IT (fallback on `getLabel()`, which for a single thumb returns
// nothing): so it is `SliderThumb` that must receive the `aria-*`. The rest
// (`data-bar`, `data-volume-slider`, etc.) goes on to the root, where the
// caller expects to find it.
const attrs = useAttrs()
const ariaAttrs = computed(() => Object.fromEntries(
  Object.entries(attrs).filter(([key]) => key.startsWith('aria-')),
))
const rootAttrs = computed(() => Object.fromEntries(
  Object.entries(attrs).filter(([key]) => !key.startsWith('aria-')),
))

// A vertical slider (the equalizer) sits in the page's scrolling direction:
// on a phone its ten 144 px bands cover most of the card, and reka-ui's
// pointerdown on anything but the thumb starts a slide at once, jumping the
// value under the finger. A swipe meant to scroll the page then rewrote gains
// by up to 12 dB. So, on a vertical slider only, a finger or a pen moves the
// value **by the thumb alone**: the root lets the browser pan
// (`touch-manipulation` instead of `touch-none`), and this capture-phase
// listener keeps a touch or pen pointerdown that is not on the thumb away from
// reka-ui, which then never starts a slide — the browser scrolls the page.
// The thumb keeps `touch-none` (dragging it moves the value) and a larger hit
// area, its `before:` pseudo-element: 24 x 40 px
// around the 16 px dot, no wider than the narrowest equalizer column (24 px
// on a phone), so it never reaches a neighbour's thumb. A mouse and the
// keyboard are untouched. A pen (on a touch laptop, say) is held to the
// finger's rule on purpose: it scrolls the page too, so tapping a vertical
// slider's track with it no longer sets a value. When the finger lands on the root's own
// padding, the event is at its target, where reka-ui's listener sits too: the
// capture pass runs first there, and stopping propagation keeps the bubble
// pass (reka-ui's) from running; `stopImmediatePropagation` also keeps out
// any other capture listener of the root.
// The rest of a gesture kept away is kept away too. The browser captures a
// touch pointer on the element it landed on by itself, so reka-ui's
// pointermove sees a capture and emits a slide move before the pan's
// `pointercancel`: that move can set a value, and it leaves reka-ui holding
// the slider's rectangle from before the scroll (cleared only by a slide
// end, which a cancelled gesture never sends) — the next thumb drag then
// read its value against where the slider used to be (measured: a drag
// down that set +11 dB).
// A new pointerdown closes whatever an earlier gesture of the same pointer id
// left behind: its up or cancel may never have reached the root (a pen
// lifted off the slider, with no implicit capture), and a stale id would
// swallow the moves of a later drag on the thumb.
// Horizontal sliders (progress, volume, alarm) keep whole-track
// drag-to-set on purpose: they lie across the scrolling direction.
const keptAway = new Set<number>()

function keepTouchForScrolling(event: PointerEvent) {
  keptAway.delete(event.pointerId)
  if (props.orientation !== "vertical") return
  if (event.pointerType !== "touch" && event.pointerType !== "pen") return
  if ((event.target as Element).closest("[data-slot=slider-thumb]")) return
  keptAway.add(event.pointerId)
  event.stopImmediatePropagation()
}

function keepGestureAway(event: PointerEvent) {
  if (!keptAway.has(event.pointerId)) return
  if (event.type !== "pointermove") keptAway.delete(event.pointerId)
  event.stopImmediatePropagation()
}
</script>

<template>
  <SliderRoot
    data-slot="slider"
    v-bind="{ ...rootAttrs, ...forwarded }"
    :class="cn(
      'relative flex w-full touch-none select-none items-center py-[19px] data-[disabled]:opacity-50',
      'data-[orientation=vertical]:w-auto data-[orientation=vertical]:touch-manipulation data-[orientation=vertical]:flex-col data-[orientation=vertical]:px-[19px] data-[orientation=vertical]:py-0',
      props.class,
    )"
    @pointerdown.capture="keepTouchForScrolling"
    @pointermove.capture="keepGestureAway"
    @pointerup.capture="keepGestureAway"
    @pointercancel.capture="keepGestureAway"
  >
    <SliderTrack data-slot="slider-track" class="relative h-1.5 w-full grow overflow-hidden rounded-full bg-muted data-[orientation=vertical]:h-full data-[orientation=vertical]:w-1.5">
      <SliderRange data-slot="slider-range" class="absolute h-full bg-primary data-[orientation=vertical]:h-auto data-[orientation=vertical]:w-full" />
    </SliderTrack>
    <SliderThumb
      data-slot="slider-thumb"
      v-bind="ariaAttrs"
      class="block size-4 shrink-0 cursor-pointer rounded-full border border-primary bg-background shadow-sm ring-ring/50 transition-[color,box-shadow] outline-none hover:ring-4 focus-visible:ring-4 disabled:pointer-events-none data-[orientation=vertical]:touch-none data-[orientation=vertical]:before:absolute data-[orientation=vertical]:before:-inset-x-1 data-[orientation=vertical]:before:-inset-y-3"
    />
  </SliderRoot>
</template>
