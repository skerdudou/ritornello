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
const { state, locale, session, settled } = usePlugins()
const uiVersion = computed(
  () => state.value.plugins.find((p) => p.name === name.value)?.ui_version ?? '',
)

/**
 * Relayed to `PluginView` so it holds its module-loading effect until
 * `/api/status` has settled once — see that prop's doc. `settled` never goes
 * back down once raised, on failure as much as on success: a transient
 * outage must not re-open the curtain on a page already showing.
 */
const statusPending = computed(() => !settled.value)

/**
 * Suffix that makes a catalog URL cacheable for good: the language it is in,
 * and the stamp of the core's session.
 *
 * Empty when either is still unknown. An unstamped URL is merely uncached; one
 * carrying an empty `v=` would be cached **for ever** under a false stamp.
 *
 * That emptiness used to be the ordinary case on a hard reload — the watch
 * below ran before `/api/status` had answered — and it costs more than a cache
 * miss: without `lang`, the core falls back to its own current interface
 * language (`AppState.locale_current`, see `admin_i18n`) rather than failing,
 * so the catalog itself was never wrong — but the URL stayed unstamped, and
 * an unstamped catalog URL is answered `no-cache` and refetched on every
 * visit instead of being cached for the session. The watch now waits for
 * `/api/status`, so this stays empty in one case only: `/api/status` itself
 * failed.
 */
function catalogQuery(locale: string, session: string): string {
  if (!locale || !session) return ''
  return `?lang=${encodeURIComponent(locale)}&v=${encodeURIComponent(session)}`
}

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
 * plugin component's **mount**.
 *
 * Building the plugin's component as soon as its module arrives shows the
 * translation **keys** — `col_num`, `btn_save` — which the real labels replace
 * a moment later; they do not have the same length, so every label of the page
 * shifts once it is already on screen. This flag is what lets the view wait for
 * both answers before building anything.
 *
 * It used to hold only the *reveal*, the component being mounted underneath
 * behind a `display: none`. That was not enough, and a bug reported from use
 * proved it: a hidden component is a running component, and a value captured
 * at mount — a Select's option text, see `PluginView` — never recovered.
 * The two requests still leave together; what is now serialised, when the
 * catalog is the slower of the two, is the plugin's own `onMounted` request.
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
  // **`settled` and `locale` are sources, not just readings**, and that is a
  // fix. Watching the route name alone, the immediate run fired before
  // `/api/status` had answered: `catalogQuery` then had no language to put in
  // the URL, so the request left unstamped. At the time this was reported —
  // before the core's admin_i18n resolved every language from its own shared
  // registry (task 4/5 of the language-packs chantier) — an unstamped request
  // meant an IPC round trip asking the plugin its own current language, and a
  // plugin that had never been told one (an admin-only plugin, or any plugin
  // before its first `SetLocale`) answered in whatever default it held,
  // typically English. Nothing re-ran the request afterwards, the language
  // not being watched, so a hard reload on a plugin page stayed in English
  // until the reader navigated away and back. Reported from use, on every
  // plugin page.
  //
  // Neither half of that mechanism exists any more: `admin_i18n` resolves
  // `lang` from the registry with no IPC at all, and falls back — absent
  // `lang` — to the core's own current interface language
  // (`AppState.locale_current`), never to a plugin's; no plugin has held a
  // language of its own since `SourceReq::SetLocale` was retired (task 11).
  // An unstamped request is honest today, just uncacheable (`no-cache`, see
  // `admin_i18n`'s own doc) — which is why this watch is still worth having:
  // waiting for `locale` lets the very first request leave already stamped.
  //
  // `session` joins them for the same reason it is in the URL: a core that
  // restarted invalidates the stamp, and the answer must be fetched again.
  [() => route.params.name, settled, locale, session],
  async ([value]) => {
    name.value = String(value ?? '')
    if (!name.value) return
    // Raised **before** the request leaves, and synchronously: a navigation to
    // another plugin must close the curtain on the spot, otherwise the
    // incoming page is revealed for one frame carrying the previous plugin's
    // catalog.
    catalogPending.value = true
    // Nothing leaves before `/api/status` has settled: that answer carries the
    // language, and asking without it gets an unstamped, uncacheable request
    // instead. Waiting cannot hang — `settled` is raised on failure as much as
    // on success (see `usePlugins.reload`), and the request then goes out
    // unstamped, which is the honest degradation: `/api/status` is down, so
    // nobody knows the language.
    if (!settled.value) return
    const localGeneration = ++generation
    // An unreachable catalog must not prevent the UI from showing: `t()` then
    // falls back on the keys, which stays readable. The log keeps the trace,
    // and the cause is **rendered on screen** by `PluginView`: it comes from
    // the body of the core's refusal (`api.get` extracts its `error` field),
    // and that is the only channel carrying it.
    let reason = ''
    const url = `/plugins/${name.value}/api/i18n${catalogQuery(locale.value, session.value)}`
    const loaded = await api.get<Catalog>(url).catch((e: unknown) => {
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
    :status-pending="statusPending"
  />
</template>
