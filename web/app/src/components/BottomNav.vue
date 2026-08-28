<script setup lang="ts">
import { ActivityLogIcon, CubeIcon, MixerHorizontalIcon, PlayIcon } from '@radix-icons/vue'
import { computed } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import { usePlugins } from '../composables/usePlugins'

// Quatre onglets fixes : la partie variable (une page par greffon) est derriere
// « Greffons ». Un seul greffon a page : l'onglet y mene tout droit, la list
// n'apporterait rien.
const { t } = useCatalog()
const { admins } = usePlugins()
const pluginsTarget = computed(() => (admins.value.length === 1 ? `/plugins/${admins.value[0]}/` : '/plugins/'))

// `/plugins/` et `/plugins/:name/` sont des routes soeurs, step l'une prefixe
// de l'autre : la correspondance inclusive d'`active-class` ne les confond
// jamais, donc l'onglet resterait eteint sur la page d'un greffon. D'ou ce
// calcul a la main sur le chemin courant, plutot que sur le mecanisme du
// routeur.
const route = useRoute()
const pluginsActive = computed(() => route.path.startsWith('/plugins/'))

const TAB = 'flex h-14 flex-col items-center justify-center gap-1 text-[11px] font-medium text-muted-foreground'
const ACTIVE = 'text-primary'
</script>

<template>
  <!-- `fixed` et non `sticky` : le `main` defile sous elle, et le
       `safe-area-inset-bottom` la degage de la barre gestuelle du phone.
       Masquee a partir de `md`, ou la nav du haut reprend. -->
  <nav
    class="fixed inset-x-0 bottom-0 z-10 grid grid-cols-4 border-t border-border bg-card pb-[env(safe-area-inset-bottom)] md:hidden"
    data-nav-basse
    :aria-label="t('nav_label')"
  >
    <RouterLink to="/" :class="TAB" :exact-active-class="ACTIVE">
      <PlayIcon class="size-5" />{{ t('nav_listen') }}
    </RouterLink>
    <!-- Classe calculee a la main, step `active-class` : `/plugins/` et
         `/plugins/:name/` sont des routes soeurs, la correspondance inclusive
         du routeur ne couvre step ce cas (voir `pluginsActive` ci-dessus). -->
    <RouterLink :to="pluginsTarget" :class="[TAB, pluginsActive ? ACTIVE : '']" data-nav-plugins>
      <CubeIcon class="size-5" />{{ t('nav_plugins') }}
    </RouterLink>
    <RouterLink to="/system" :class="TAB" :exact-active-class="ACTIVE">
      <ActivityLogIcon class="size-5" />{{ t('system_title') }}
    </RouterLink>
    <RouterLink to="/config" :class="TAB" :exact-active-class="ACTIVE">
      <MixerHorizontalIcon class="size-5" />{{ t('nav_settings') }}
    </RouterLink>
  </nav>
</template>
