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
// Shared with `ConfigView` at module level: this is what makes a toggle made
// on the configuration page remove or restore the menu entry here, without
// reloading the page. See `usePlugins`, which writes the previous default.
const { admins, refresh: refreshPlugins } = usePlugins()

/**
 * Classes shared by the nav links. The underline is an `after` pseudo-element
 * scaled horizontally, not a single bar that would slide from one tab to the
 * next: that one would require measuring the active element's position in JS,
 * a measurement to redo when the nav wraps, when the language changes (labels
 * change width) and when the admin plugins arrive late, which is only known
 * after `/api/status`. One pseudo-element per link ignores all three problems.
 *
 * `origin-left`: the stroke is drawn under the word in reading direction,
 * rather than opening from the center. `motion-reduce` makes it instantaneous
 * for whoever asked their system for fewer animations.
 */
const LINK = [
  'relative py-1 transition-colors hover:text-foreground',
  'after:absolute after:inset-x-0 after:-bottom-px after:h-0.5 after:rounded-full',
  'after:origin-left after:scale-x-0 after:bg-primary',
  'after:transition-transform after:duration-200 motion-reduce:after:transition-none',
].join(' ')

/**
 * Marker of the current page. `exact-active-class` and not `active-class`:
 * inclusive matching would make the `/` link active on **every** page.
 * Exactness suits every link here, the router having no sub-route (see
 * `router.ts`) — one nav link is exactly one route.
 *
 * The underline duplicates information `RouterLink` already carries as
 * `aria-current="page"`: the visual cue adds to the semantics, it does not
 * replace them.
 */
const LINK_ACTIVE = 'text-foreground after:scale-x-100'

onMounted(async () => {
  // Before the catalog's `await`, not after: a slow catalog must not delay the
  // first sample. Started here — the root of the SPA — and not in
  // `SystemView`, so that the history exists before the first visit to the
  // system tab, survives navigation, and has a single starting point (two
  // would fight over the same timer).
  useMetrics().start()
  await reload()
  await refreshPlugins()
})
</script>

<template>
  <div class="min-h-screen">
    <header class="border-b border-border">
      <nav class="mx-auto flex max-w-5xl items-center gap-4 px-4 py-3">
        <!-- The brand is the home link, so it carries the same marker:
             without it, the home page would be the only one with nothing
             underlined. -->
        <RouterLink to="/" :class="[LINK, 'font-semibold']" :exact-active-class="LINK_ACTIVE">
          Ritornello
        </RouterLink>
        <!-- Hidden below `md`: the fixed bottom bar (`BottomNav`) takes over
             on phones, with its four fixed tabs. -->
        <div class="hidden items-center gap-4 md:flex" data-top-nav>
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
          <!-- `first-letter:uppercase` in CSS, not in i18n: these names come
               from plugins.toml (including third-party plugins), no catalog
               could cover them, and adding a label field to the plugin
               protocol would be disproportionate for a single capital
               letter. -->
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
    <!-- Centered at the bottom and colored by type: on a living-room screen,
         a discreet notification in a corner goes unnoticed, and "saved" must
         be told apart from a refusal without having to read. `rich-colors` is
         what gives vue-sonner's green and red; without it the two outcomes
         look alike. -->
    <Toaster position="bottom-center" rich-colors />
  </div>
</template>
