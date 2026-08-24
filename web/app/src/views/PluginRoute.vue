<script setup lang="ts">
import { api, type Catalog } from '@ritornello/ui'
import { ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import PluginView from './PluginView'

const route = useRoute()
const name = ref('')
const catalog = ref<Catalog>({})
/**
 * Cause d'un refus du cœur, telle qu'il la porte désormais.
 *
 * Le module d'IHM est chargé par `import()`, dont l'échec ne livre aucun corps
 * exploitable : cet appel-ci est le **seul** qui puisse dire pourquoi un plugin
 * ne répond pas. Au premier chargement d'une page dont le plugin est mort,
 * l'écran n'affichait qu'« IHM du plugin indisponible » — la cause partait dans
 * un `console.warn`, au moment précis où elle compte.
 *
 * Vide quand tout va bien, ou quand l'échec n'a pas de cause à donner.
 */
const cause = ref('')

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
    // `t()` retombe alors sur les cles, ce qui reste lisible. Le journal garde
    // la trace, et la cause est **rendue a l'ecran** par `PluginView` : elle
    // vient du corps du refus du coeur (`api.get` en extrait le champ `error`),
    // et c'est le seul canal qui la porte.
    let motif = ''
    const charge = await api.get<Catalog>(`/plugins/${name.value}/api/i18n`).catch((e: unknown) => {
      console.warn(`plugin ${name.value}: catalogue i18n indisponible`, e)
      motif = e instanceof Error ? e.message : String(e)
      return {}
    })
    // Sous la meme garde de generation que le catalogue : une cause en retard
    // ne doit pas s'afficher sous l'admin d'un autre plugin.
    if (generationLocale === generation) {
      catalog.value = charge
      cause.value = motif
    }
  },
  { immediate: true },
)
</script>

<template>
  <PluginView v-if="name" :key="name" :name="name" :catalog="catalog" :cause="cause" />
</template>
