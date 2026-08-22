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
  <!-- `data-[state=inactive]:hidden` n'est pas une precaution : avec
       `force-mount`, reka-ui rend le panneau inactif **sans** attribut
       `hidden` (il laisse le consommateur le masquer, pour pouvoir animer la
       transition). Mesure : le panneau inactif ne porte que
       `data-state="inactive"`. Sans cette classe, tous les panneaux
       s'affichent en meme temps et les onglets n'ont plus aucun effet visible
       -- exactement le defaut signale a l'usage. -->
  <TabsContent
    data-slot="tabs-content"
    v-bind="forwardedProps"
    :class="cn('flex-1 outline-none data-[state=inactive]:hidden', props.class)"
  >
    <slot />
  </TabsContent>
</template>
