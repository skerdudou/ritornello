<script setup lang="ts">
/**
 * Pass-through for reka-ui's `ConfigProvider`, exported by the kit for one
 * reason that has nothing to do with styling: **there must be a single
 * reka-ui instance in the page**.
 *
 * The kit is the only bundle that carries reka-ui (`ui-kit.js`, reached
 * through the import map — see web/app/vite.config.ts). A shell or a plugin
 * importing `reka-ui` directly would bundle a second copy, with its own
 * `provide`/`inject` keys and its own shared state: the provider from one copy
 * would configure nothing in the components of the other, silently. Going
 * through the kit is what makes the provider and the `Select` it configures
 * the same reka-ui.
 *
 * Deliberately a bare forwarder: it renders only its slot (as reka-ui's does,
 * no wrapper element) and takes no decision. The decision — which config the
 * appliance needs and why — belongs to whoever also owns the stylesheet it has
 * to agree with, i.e. the shell's App.vue.
 */
import type { ConfigProviderProps } from "reka-ui"
import { ConfigProvider } from "reka-ui"

const props = defineProps<ConfigProviderProps>()
</script>

<template>
  <ConfigProvider v-bind="props">
    <slot />
  </ConfigProvider>
</template>
