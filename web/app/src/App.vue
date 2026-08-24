<script setup lang="ts">
import { api, Toaster } from '@ritornello/ui'
import { onMounted, ref } from 'vue'
import { RouterLink, RouterView } from 'vue-router'
import ThemeToggle from './components/ThemeToggle.vue'
import { useCatalog } from './composables/useCatalog'
import { useMetriques } from './composables/useMetriques'
import type { StatusPayload } from './types'

const { t, reload } = useCatalog()
const admins = ref<string[]>([])

/**
 * Classes communes des liens de la nav. Le soulignement est un pseudo-élément
 * `after` mis à l'échelle horizontalement, et non une barre unique qui
 * glisserait d'un onglet à l'autre : celle-là exigerait de mesurer la position
 * de l'élément actif en JS, mesure à refaire au retour à la ligne de la nav,
 * au changement de langue (les libellés changent de largeur) et à l'arrivée
 * tardive des plugins admin, qui n'est connue qu'après `/api/status`. Un
 * pseudo-élément par lien ignore ces trois problèmes.
 *
 * `origin-left` : le trait s'écrit sous le mot dans le sens de la lecture,
 * plutôt que de s'ouvrir depuis le centre. `motion-reduce` le rend instantané
 * pour qui a demandé moins d'animations dans son système.
 */
const LIEN = [
  'relative py-1 transition-colors hover:text-foreground',
  'after:absolute after:inset-x-0 after:-bottom-px after:h-0.5 after:rounded-full',
  'after:origin-left after:scale-x-0 after:bg-primary',
  'after:transition-transform after:duration-200 motion-reduce:after:transition-none',
].join(' ')

/**
 * Marqueur de la page courante. `exact-active-class` et non `active-class` :
 * la correspondance inclusive rendrait le lien `/` actif sur **toutes** les
 * pages. L'exactitude convient à chaque lien ici, le routeur n'ayant aucune
 * sous-route (voir `router.ts`) — un lien de nav vaut exactement une route.
 *
 * Le soulignement double une information que `RouterLink` porte déjà en
 * `aria-current="page"` : le repère visuel s'ajoute à la sémantique, il ne la
 * remplace pas.
 */
const LIEN_ACTIF = 'text-foreground after:scale-x-100'

onMounted(async () => {
  // Avant l'`await` du catalogue, et non après : un catalogue lent ne doit pas
  // retarder le premier échantillon. Amorcé ici — la racine de la SPA — et pas
  // dans `SystemView`, pour que l'historique existe avant la première visite de
  // l'onglet système, survive à la navigation, et n'ait qu'un seul point de
  // départ (deux se disputeraient le même minuteur).
  useMetriques().demarrer()
  await reload()
  // Un `/api/status` injoignable prive silencieusement la navigation de tous
  // les plugins admin — le symptome le plus difficile a attribuer sans
  // diagnostic, la page ayant l'air normale par ailleurs.
  const s = await api.get<StatusPayload>('/api/status').catch((e) => {
    console.warn('GET /api/status indisponible : navigation sans les plugins admin', e)
    return null
  })
  admins.value = (s?.plugins ?? []).filter((p) => p.admin).map((p) => p.name)
})
</script>

<template>
  <div class="min-h-screen">
    <header class="border-b border-border">
      <nav class="mx-auto flex max-w-5xl items-center gap-4 px-4 py-3">
        <!-- La marque est le lien de l'accueil, donc elle porte le même
             marqueur : sans elle, la page d'accueil serait la seule sans rien
             de souligné. -->
        <RouterLink to="/" :class="[LIEN, 'font-semibold']" :exact-active-class="LIEN_ACTIF">
          Ritornello
        </RouterLink>
        <RouterLink
          to="/config"
          :class="[LIEN, 'text-sm text-muted-foreground']"
          :exact-active-class="LIEN_ACTIF"
        >
          {{ t('config_title') }}
        </RouterLink>
        <RouterLink
          to="/system"
          :class="[LIEN, 'text-sm text-muted-foreground']"
          :exact-active-class="LIEN_ACTIF"
        >
          {{ t('system_title') }}
        </RouterLink>
        <!-- `first-letter:uppercase` en CSS, pas en i18n : ces noms viennent
             de plugins.toml (y compris des plugins tiers), aucun catalogue
             ne pourrait les couvrir, et ajouter un champ de libellé au
             protocole des plugins serait disproportionné pour une seule
             capitale. -->
        <RouterLink
          v-for="name in admins"
          :key="name"
          :to="`/plugins/${name}/`"
          :class="[LIEN, 'text-sm text-muted-foreground first-letter:uppercase']"
          :exact-active-class="LIEN_ACTIF"
        >
          {{ name }}
        </RouterLink>
        <ThemeToggle class="ml-auto" />
      </nav>
    </header>
    <main class="mx-auto max-w-5xl px-4 py-6">
      <RouterView />
    </main>
    <!-- Centrees en bas et colorees par type : sur un ecran de salon, une
         notification discrete dans un coin passe inapercue, et « enregistre »
         doit se distinguer d'un refus sans avoir a lire. `rich-colors` est ce
         qui donne le vert et le rouge de vue-sonner ; sans lui les deux issues
         se ressemblent. -->
    <Toaster position="bottom-center" rich-colors />
  </div>
</template>
