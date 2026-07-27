import { api, createT, type Catalog } from '@ritornello/ui'
import { computed, ref } from 'vue'

// Catalogue du coeur, partage par toutes les vues. Recharge au changement de
// langue (la page ne se recharge plus entierement comme autrefois).
const catalog = ref<Catalog>({})

export function useCatalog() {
  const t = computed(() => createT(catalog.value))
  async function reload(): Promise<void> {
    // Un échec transitoire garde le catalogue en place : l'écraser par `{}`
    // faisait basculer toute l'IHM (nav, cartes, télécommande) en clés brutes
    // jusqu'à un rechargement manuel — pire que de rester une langue en
    // retard. Même convention que `chargerTout` de la page de statut.
    catalog.value = await api.get<Catalog>('/api/i18n').catch(() => catalog.value)
  }
  return { t, reload }
}
