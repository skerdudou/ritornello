import { api, createT, type Catalog } from '@ritornello/ui'
import { computed, readonly, ref } from 'vue'

// Catalog of the core, shared by all the views. Reloaded on a language change
// (the page no longer fully reloads as it used to).
const catalog = ref<Catalog>({})

/**
 * Whether the catalog request has come back at least once.
 *
 * What it is for: a view rendered before it has a catalog shows translation
 * **keys**, and most of them recover on the re-render that follows — but not
 * all. The kit's `SelectItemText` registers an item's text with its Select
 * when the item mounts and never re-reads it, so a dropdown built that early
 * keeps showing `audio_default_device` or `startup_on` for the life of the
 * page. That was reported on the configuration page, and patching each
 * dropdown would only have held until the next one was written.
 *
 * **One-way**: once raised it never falls again. `reload()` also runs on a
 * language change, and lowering this then would blank the whole page every
 * time the language is switched.
 *
 * **Settled means answered *or* failed**, never an indefinite wait: a page
 * withheld forever because `/api/i18n` is down would be far worse than one
 * showing keys. Same rule, and for the same reason, as `statusPending` in
 * `PluginView`.
 */
const settled = ref(false)

export function useCatalog() {
  const t = computed(() => createT(catalog.value))
  async function reload(): Promise<void> {
    try {
      // A transient failure keeps the catalog in place: overwriting it with `{}`
      // switched the whole UI (nav, cards, remote) to raw keys until a manual
      // reload — worse than staying one language behind. Same convention as
      // `loadAll` of the status page.
      catalog.value = await api.get<Catalog>('/api/i18n').catch(() => catalog.value)
    } finally {
      settled.value = true
    }
  }
  return { t, reload, settled: readonly(settled) }
}

/**
 * Puts the shared state back to what a fresh page load holds.
 *
 * For the tests alone, and necessary because `catalog` and `settled` live at
 * module level: without it, the first test to let a catalog land would leave
 * every later one believing the shell is ready — the class of leak that once
 * made a poll escape from one test into the next (see `useMetrics`, which
 * exports the same escape hatch for the same reason).
 */
export function resetCatalog(): void {
  catalog.value = {}
  settled.value = false
}
