<script setup lang="ts">
import type { TabsContentProps } from "reka-ui"
import type { HTMLAttributes } from "vue"
import { reactiveOmit } from "@vueuse/core"
import { TabsContent, useForwardProps } from "reka-ui"
import { cn } from "@/lib/utils"

const props = defineProps<TabsContentProps & { class?: HTMLAttributes["class"] }>()

const delegatedProps = reactiveOmit(props, "class")
const forwardedProps = useForwardProps(delegatedProps)
</script>

<template>
  <!-- `data-[state=inactive]:hidden` is not a precaution: with `force-mount`,
       reka-ui renders the inactive panel **without** a `hidden` attribute (it
       leaves it to the consumer to hide it, so the transition can be
       animated). Measured: the inactive panel only carries
       `data-state="inactive"`. Without this class, all panels show at the
       same time and the tabs have no visible effect anymore -- exactly the
       defect reported in use. -->
  <TabsContent
    data-slot="tabs-content"
    v-bind="forwardedProps"
    :class="cn('flex-1 outline-none data-[state=inactive]:hidden', props.class)"
  >
    <slot />
  </TabsContent>
</template>
