<script setup lang="ts">
import { api, type Catalog } from '@ritornello/ui'
import { ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import PluginView from './PluginView'

const route = useRoute()
const name = ref('')
const catalog = ref<Catalog>({})

watch(
  () => route.params.name,
  async (valeur) => {
    name.value = String(valeur ?? '')
    if (!name.value) return
    // Un catalogue injoignable ne doit pas empecher l'IHM de s'afficher :
    // `t()` retombe alors sur les cles, ce qui reste lisible — mais ce repli
    // silencieux doit laisser une trace pour ne pas masquer un vrai probleme
    // cote plugin.
    catalog.value = await api.get<Catalog>(`/plugins/${name.value}/api/i18n`).catch((e) => {
      console.warn(`plugin ${name.value}: catalogue i18n indisponible`, e)
      return {}
    })
  },
  { immediate: true },
)
</script>

<template>
  <PluginView v-if="name" :key="name" :name="name" :catalog="catalog" />
</template>
