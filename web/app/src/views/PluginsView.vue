<script setup lang="ts">
import { Badge, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { ChevronRightIcon } from '@radix-icons/vue'
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import { usePlugins } from '../composables/usePlugins'

// The variable part of the navigation, laid out as a list: on a phone the
// bottom bar has four fixed tabs, and this is where "Plugins" lands.
// Same source and same order as the top links (`usePlugins().admins`, hence
// `/api/status`, hence plugins.toml) — no priority inferred elsewhere.
const { t } = useCatalog()
const { admins, state } = usePlugins()
const connected = (name: string) => state.value.plugins.some((p) => p.name === name && p.connected)
const list = computed(() => admins.value)
</script>

<template>
  <Card>
    <CardHeader><CardTitle>{{ t('plugins_list_title') }}</CardTitle></CardHeader>
    <CardContent>
      <ul v-if="list.length" class="divide-y divide-border" data-plugins-list>
        <li v-for="name in list" :key="name">
          <RouterLink :to="`/plugins/${name}/`" class="flex min-h-14 items-center gap-3 py-2 hover:text-foreground">
            <span class="flex-1 font-medium first-letter:uppercase">{{ name }}</span>
            <!-- Same word, same color as the state badge of ConfigView: the
                 key `plugin_disconnected` does not exist in the catalog, and
                 `unavailable` is already the one that designates an unreachable
                 plugin over there — no terminological duplicate between the two pages. -->
            <Badge v-if="!connected(name)" variant="destructive">{{ t('unavailable') }}</Badge>
            <ChevronRightIcon class="size-4 text-muted-foreground" />
          </RouterLink>
        </li>
      </ul>
      <p v-else class="text-sm text-muted-foreground" data-plugins-vide>{{ t('plugins_list_empty') }}</p>
    </CardContent>
  </Card>
</template>
