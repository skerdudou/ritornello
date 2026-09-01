<script setup lang="ts">
import { api, type Catalog } from '@ritornello/ui'
import { ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import PluginView from './PluginView'

const route = useRoute()
const name = ref('')
const catalog = ref<Catalog>({})
/**
 * Cause of a refusal by the core, as it now carries it.
 *
 * The UI module is loaded with `import()`, whose failure delivers no usable
 * body: this call is the **only** one that can say why a plugin does not
 * answer. On the first load of a page whose plugin is dead, the screen only
 * showed "plugin UI unavailable" — the cause went into a `console.warn`, at
 * the very moment it mattered.
 *
 * Empty when all is well, or when the failure has no cause to give.
 */
const cause = ref('')

// Generation counter: the `watch` is asynchronous, and a fast navigation
// radio → generic-input with a slow GET let the catalog of the first plugin
// settle **after** that of the second — the displayed admin then ran with the
// catalog of another plugin. `PluginView` has the same guard for the module;
// this one protects the catalog.
let generation = 0

watch(
  () => route.params.name,
  async (value) => {
    name.value = String(value ?? '')
    if (!name.value) return
    const localGeneration = ++generation
    // An unreachable catalog must not prevent the UI from showing: `t()` then
    // falls back on the keys, which stays readable. The log keeps the trace,
    // and the cause is **rendered on screen** by `PluginView`: it comes from
    // the body of the core's refusal (`api.get` extracts its `error` field),
    // and that is the only channel carrying it.
    let reason = ''
    const loaded = await api.get<Catalog>(`/plugins/${name.value}/api/i18n`).catch((e: unknown) => {
      console.warn(`plugin ${name.value}: i18n catalog unavailable`, e)
      reason = e instanceof Error ? e.message : String(e)
      return {}
    })
    // Under the same generation guard as the catalog: a late cause must not
    // show up under the admin of another plugin.
    if (localGeneration === generation) {
      catalog.value = loaded
      cause.value = reason
    }
  },
  { immediate: true },
)
</script>

<template>
  <PluginView v-if="name" :key="name" :name="name" :catalog="catalog" :cause="cause" />
</template>
