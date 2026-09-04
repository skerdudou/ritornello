<script setup lang="ts">
import { api, type Catalog } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { usePlugins } from '../composables/usePlugins'
import PluginView from './PluginView'

const route = useRoute()
const name = ref('')
const catalog = ref<Catalog>({})

/**
 * Fingerprint of the current plugin's UI assets, as `/api/status` last
 * reported it — the same state the nav and the settings table already share.
 * Absent (`''`) for a plugin that announced none, or before the first
 * `/api/status` answer settles.
 */
const { state } = usePlugins()
const uiVersion = computed(
  () => state.value.plugins.find((p) => p.name === name.value)?.ui_version ?? '',
)
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

/**
 * Whether the catalog request is still in flight, for `PluginView` to hold the
 * reveal.
 *
 * Mounting the plugin's component as soon as its module arrives shows the
 * translation **keys** — `col_num`, `btn_save` — which the real labels replace
 * a moment later; they do not have the same length, so every label of the page
 * shifts once it is already on screen. This flag is what lets the view wait for
 * both answers before drawing anything. The two requests are unchanged and
 * still leave together: nothing is serialised, only the curtain is held.
 *
 * Starts raised: at the very first render the request has not been sent yet,
 * and a flag that started down would reveal an empty catalog before raising
 * itself.
 */
const catalogPending = ref(true)

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
    // Raised **before** the request leaves, and synchronously: a navigation to
    // another plugin must close the curtain on the spot, otherwise the
    // incoming page is revealed for one frame carrying the previous plugin's
    // catalog.
    catalogPending.value = true
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
      // A refusal settles the catalog just as much as a success does: the flag
      // says "the answer is in", not "the answer is good". Leaving it raised
      // on a 502 would hold the curtain shut for good — on the very page whose
      // job is then to display the cause of that refusal.
      catalogPending.value = false
    }
  },
  { immediate: true },
)
</script>

<template>
  <PluginView
    v-if="name"
    :key="name"
    :name="name"
    :catalog="catalog"
    :cause="cause"
    :catalog-pending="catalogPending"
    :ui-version="uiVersion"
  />
</template>
