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

// Une seule poignee, toujours : les deux usages du projet (progression, volume)
// sont des valeurs scalaires. `py-[19px]` : 19 + 6 + 19 = 44 px de zone de
// contact autour d'une piste de 6 px — la cible minimale au doigt, portee par
// le padding et non par la piste, qui garde sa finesse.
const props = defineProps<SliderRootProps & { class?: HTMLAttributes["class"] }>()
const emits = defineEmits<SliderRootEmits>()
const delegatedProps = reactiveOmit(props, "class")
const forwarded = useForwardPropsEmits(delegatedProps, emits)

// `aria-label` n'est pas une prop declaree de `SliderRootProps` : sans ce
// tri, elle tombe dans `$attrs` du composant et `v-bind="forwarded"` la
// laisse filer vers `SliderRoot`, qui la pose sur son `<span>` englobant —
// pas sur la poignee. Or `SliderThumbImpl` calcule SON `aria-label` a partir
// des attrs qui LUI sont passes (repli sur `getLabel()`, qui pour une seule
// poignee ne renvoie rien) : c'est donc `SliderThumb` qui doit recevoir les
// `aria-*`. Le reste (`data-barre`, `data-volume-curseur`, etc.) continue
// vers le root, la ou l'appelant s'attend a le retrouver.
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
