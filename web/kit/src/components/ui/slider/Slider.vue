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

// A single thumb, always: the project's two usages (progress, volume) are
// scalar values. `py-[19px]`: 19 + 6 + 19 = 44 px of touch area around a 6 px
// track — the minimum finger target, carried by the padding and not by the
// track, which keeps its thinness.
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
// (`data-barre`, `data-volume-curseur`, etc.) goes on to the root, where the
// caller expects to find it.
const attrs = useAttrs()
const ariaAttrs = computed(() => Object.fromEntries(
  Object.entries(attrs).filter(([key]) => key.startsWith('aria-')),
))
const rootAttrs = computed(() => Object.fromEntries(
  Object.entries(attrs).filter(([key]) => !key.startsWith('aria-')),
))
</script>

<template>
  <SliderRoot
    data-slot="slider"
    v-bind="{ ...rootAttrs, ...forwarded }"
    :class="cn(
      'relative flex w-full touch-none select-none items-center py-[19px] data-[disabled]:opacity-50',
      props.class,
    )"
  >
    <SliderTrack data-slot="slider-track" class="relative h-1.5 w-full grow overflow-hidden rounded-full bg-muted">
      <SliderRange data-slot="slider-range" class="absolute h-full bg-primary" />
    </SliderTrack>
    <SliderThumb
      data-slot="slider-thumb"
      v-bind="ariaAttrs"
      class="block size-4 shrink-0 cursor-pointer rounded-full border border-primary bg-background shadow-sm ring-ring/50 transition-[color,box-shadow] outline-none hover:ring-4 focus-visible:ring-4 disabled:pointer-events-none"
    />
  </SliderRoot>
</template>
