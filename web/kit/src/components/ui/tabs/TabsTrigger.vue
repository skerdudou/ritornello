<script setup lang="ts">
import type { TabsTriggerProps } from "reka-ui"
import type { HTMLAttributes } from "vue"
import { reactiveOmit } from "@vueuse/core"
import { TabsTrigger, useForwardProps } from "reka-ui"
import { cn } from "@/lib/utils"

const props = defineProps<TabsTriggerProps & { class?: HTMLAttributes["class"] }>()

const delegatedProps = reactiveOmit(props, "class")
const forwardedProps = useForwardProps(delegatedProps)
</script>

<template>
  <TabsTrigger
    data-slot="tabs-trigger"
    v-bind="forwardedProps"
    :class="cn(
      // Souligne plutot que pilule : la barre d'onglets coiffe une page deja
      // dense, et un bandeau gris a coins arrondis y pesait plus que le
      // contenu qu'il annonce. Le trait actif suffit a dire ou l'on est.
      `data-[state=active]:border-foreground data-[state=active]:text-foreground hover:text-foreground focus-visible:ring-ring/50 -mb-px inline-flex items-center justify-center gap-1.5 border-b-2 border-transparent px-1 pb-2 text-sm font-medium whitespace-nowrap transition-colors cursor-pointer outline-none focus-visible:ring-3 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4`,
      props.class,
    )"
  >
    <slot />
  </TabsTrigger>
</template>
