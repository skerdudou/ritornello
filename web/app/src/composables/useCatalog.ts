import { api, createT, type Catalog } from '@ritornello/ui'
import { computed, ref } from 'vue'

// Catalogue du coeur, partage par toutes les vues. Recharge au changement de
// langue (la page ne se recharge plus entierement comme autrefois).
const catalog = ref<Catalog>({})

export function useCatalog() {
  const t = computed(() => createT(catalog.value))
  async function reload(): Promise<void> {
    catalog.value = await api.get<Catalog>('/api/i18n').catch(() => ({}))
  }
  return { t, reload }
}
