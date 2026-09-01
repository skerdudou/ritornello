<script setup lang="ts">
import { ActivityLogIcon, CubeIcon, MixerHorizontalIcon, PlayIcon } from '@radix-icons/vue'
import { computed } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import { usePlugins } from '../composables/usePlugins'

// Four fixed tabs: the variable part (one page per plugin) sits behind
// "Plugins". A single plugin with a page: the tab leads straight to it, the
// list would add nothing.
const { t } = useCatalog()
const { admins } = usePlugins()
const pluginsTarget = computed(() => (admins.value.length === 1 ? `/plugins/${admins.value[0]}/` : '/plugins/'))

// `/plugins/` and `/plugins/:name/` are sibling routes, not one a prefix of
// the other: the inclusive matching of `active-class` never conflates them,
// so the tab would stay off on a plugin's page. Hence this hand computation on
// the current path, rather than relying on the router's mechanism.
const route = useRoute()
const pluginsActive = computed(() => route.path.startsWith('/plugins/'))

const TAB = 'flex h-14 flex-col items-center justify-center gap-1 text-[11px] font-medium text-muted-foreground'
const ACTIVE = 'text-primary'
</script>

<template>
  <!-- `fixed` and not `sticky`: the `main` scrolls underneath it, and the
       `safe-area-inset-bottom` keeps it clear of the phone's gesture bar.
       Hidden from `md` up, where the top nav takes over. -->
  <nav
    class="fixed inset-x-0 bottom-0 z-10 grid grid-cols-4 border-t border-border bg-card pb-[env(safe-area-inset-bottom)] md:hidden"
    data-bottom-nav
    :aria-label="t('nav_label')"
  >
    <RouterLink to="/" :class="TAB" :exact-active-class="ACTIVE">
      <PlayIcon class="size-5" />{{ t('nav_listen') }}
    </RouterLink>
    <!-- Class computed by hand, not `active-class`: `/plugins/` and
         `/plugins/:name/` are sibling routes, the router's inclusive matching
         does not cover this case (see `pluginsActive` above). -->
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
