<script setup lang="ts">
import { api, type Catalog } from '@ritornello/ui'
import { ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import PluginView from './PluginView'

const route = useRoute()
const name = ref('')
const catalog = ref<Catalog>({})

// Compteur de génération : le `watch` est asynchrone, et une navigation
// rapide radio → generic-input avec un GET lent laissait le catalogue du
// premier plugin s'installer **après** celui du second — l'admin affichée
// tournait alors avec le catalogue d'un autre plugin. `PluginView` a la même
// garde pour le module ; celle-ci protège le catalogue.
let generation = 0

watch(
  () => route.params.name,
  async (valeur) => {
    name.value = String(valeur ?? '')
    if (!name.value) return
    const generationLocale = ++generation
    // Un catalogue injoignable ne doit pas empecher l'IHM de s'afficher :
    // `t()` retombe alors sur les cles, ce qui reste lisible — mais ce repli
    // silencieux doit laisser une trace pour ne pas masquer un vrai probleme
    // cote plugin.
    const charge = await api.get<Catalog>(`/plugins/${name.value}/api/i18n`).catch((e) => {
      console.warn(`plugin ${name.value}: catalogue i18n indisponible`, e)
      return {}
    })
    if (generationLocale === generation) catalog.value = charge
  },
  { immediate: true },
)
</script>

<template>
  <PluginView v-if="name" :key="name" :name="name" :catalog="catalog" />
</template>
