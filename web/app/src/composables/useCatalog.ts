import { api, createT, type Catalog } from '@ritornello/ui'
import { computed, ref } from 'vue'

// Catalog of the core, shared by all the views. Reloaded on a language change
// (the page no longer fully reloads as it used to).
const catalog = ref<Catalog>({})

export function useCatalog() {
  const t = computed(() => createT(catalog.value))
  async function reload(): Promise<void> {
    // A transient failure keeps the catalog in place: overwriting it with `{}`
    // switched the whole UI (nav, cards, remote) to raw keys until a manual
    // reload — worse than staying one language behind. Same convention as
    // `loadAll` of the status page.
    catalog.value = await api.get<Catalog>('/api/i18n').catch(() => catalog.value)
  }
  return { t, reload }
}
