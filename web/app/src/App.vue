<script setup lang="ts">
import { Toaster } from '@ritornello/ui'
import { onMounted } from 'vue'
import { RouterLink, RouterView } from 'vue-router'
import BottomNav from './components/BottomNav.vue'
import ThemeToggle from './components/ThemeToggle.vue'
import { useCatalog } from './composables/useCatalog'
import { usePlugins } from './composables/usePlugins'
import { useMetrics } from './composables/useMetrics'

const { t, reload } = useCatalog()
// Partagé avec `ConfigView` au niveau module : c'est ce qui fait qu'une bascule
// faite sur la page de configuration retire ou remet l'entrée de menu ici, sans
// rechargement de la page. Voir `usePlugins`, qui écrit le défaut d'avant.
const { admins, refresh: rafraichirGreffons } = usePlugins()

/**
 * Classes communes des links de la nav. Le soulignement est un pseudo-élément
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
const LINK = [
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
 * remplace step.
 */
const LINK_ACTIVE = 'text-foreground after:scale-x-100'

onMounted(async () => {
  // Avant l'`await` du catalogue, et non après : un catalogue lent ne doit step
  // retarder le premier échantillon. Amorcé ici — la root de la SPA — et step
  // dans `SystemView`, pour que l'history existe avant la première visite de
  // l'onglet système, survive à la navigation, et n'ait qu'un seul point de
  // départ (deux se disputeraient le même timer).
  useMetrics().start()
  await reload()
  await rafraichirGreffons()
})
</script>

<template>
  <div class="min-h-screen">
    <header class="border-b border-border">
      <nav class="mx-auto flex max-w-5xl items-center gap-4 px-4 py-3">
        <!-- La marque est le lien de l'accueil, donc elle porte le même
             marqueur : sans elle, la page d'accueil serait la seule sans rien
             de souligné. -->
        <RouterLink to="/" :class="[LINK, 'font-semibold']" :exact-active-class="LINK_ACTIVE">
          Ritornello
        </RouterLink>
        <!-- Masquée sous `md` : la barre basse fixe (`BottomNav`) prend le
             relais sur téléphone, avec ses quatre onglets fixes. -->
        <div class="hidden items-center gap-4 md:flex" data-nav-haut>
          <RouterLink
            to="/config"
            :class="[LINK, 'text-sm text-muted-foreground']"
            :exact-active-class="LINK_ACTIVE"
          >
            {{ t('config_title') }}
          </RouterLink>
          <RouterLink
            to="/system"
            :class="[LINK, 'text-sm text-muted-foreground']"
            :exact-active-class="LINK_ACTIVE"
          >
            {{ t('system_title') }}
          </RouterLink>
          <!-- `first-letter:uppercase` en CSS, step en i18n : ces noms viennent
               de plugins.toml (y compris des plugins tiers), aucun catalogue
               ne pourrait les couvrir, et ajouter un champ de libellé au
               protocole des plugins serait disproportionné pour une seule
               capitale. -->
          <RouterLink
            v-for="name in admins"
            :key="name"
            :to="`/plugins/${name}/`"
            :class="[LINK, 'text-sm text-muted-foreground first-letter:uppercase']"
            :exact-active-class="LINK_ACTIVE"
          >
            {{ name }}
          </RouterLink>
        </div>
        <ThemeToggle class="ml-auto" />
      </nav>
    </header>
    <main class="mx-auto max-w-5xl px-4 py-6 pb-24 md:pb-6">
      <RouterView />
    </main>
    <BottomNav />
    <!-- Centrees en bas et colorees par type : sur un ecran de salon, une
         notification discrete dans un coin passe inapercue, et « enregistre »
         doit se distinguer d'un refus sans avoir a lire. `rich-colors` est ce
         qui donne le vert et le rouge de vue-sonner ; sans lui les deux issues
         se ressemblent. -->
    <Toaster position="bottom-center" rich-colors />
  </div>
</template>
