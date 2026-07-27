<script setup lang="ts">
import { api, Toaster } from '@ritornello/ui'
import { onMounted, ref } from 'vue'
import { RouterLink, RouterView } from 'vue-router'
import ThemeToggle from './components/ThemeToggle.vue'
import { useCatalog } from './composables/useCatalog'
import type { StatusPayload } from './types'

const { t, reload } = useCatalog()
const admins = ref<string[]>([])

onMounted(async () => {
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
      <nav class="mx-auto flex max-w-3xl items-center gap-4 px-4 py-3">
        <RouterLink to="/" class="font-semibold">ritornello</RouterLink>
        <RouterLink to="/status" class="text-sm text-muted-foreground">{{ t('status_title') }}</RouterLink>
        <RouterLink
          v-for="name in admins"
          :key="name"
          :to="`/plugins/${name}/`"
          class="text-sm text-muted-foreground"
        >
          {{ name }}
        </RouterLink>
        <ThemeToggle class="ml-auto" />
      </nav>
    </header>
    <main class="mx-auto max-w-3xl px-4 py-6">
      <RouterView />
    </main>
    <Toaster />
  </div>
</template>
