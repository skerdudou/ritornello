<script setup lang="ts">
import { Badge, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { ChevronRightIcon } from '@radix-icons/vue'
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import { useGreffons } from '../composables/useGreffons'

// La partie variable de la navigation, rangee dans une liste : sur telephone
// la barre basse a quatre onglets fixes, et c'est ici qu'atterrit « Greffons ».
// Meme source et meme ordre que les liens du haut (`useGreffons().admins`, donc
// `/api/status`, donc plugins.toml) — aucune priorite deduite ailleurs.
const { t } = useCatalog()
const { admins, etat } = useGreffons()
const connecte = (nom: string) => etat.value.plugins.some((p) => p.name === nom && p.connected)
const liste = computed(() => admins.value)
</script>

<template>
  <Card>
    <CardHeader><CardTitle>{{ t('plugins_list_title') }}</CardTitle></CardHeader>
    <CardContent>
      <ul v-if="liste.length" class="divide-y divide-border" data-plugins-liste>
        <li v-for="name in liste" :key="name">
          <RouterLink :to="`/plugins/${name}/`" class="flex min-h-14 items-center gap-3 py-2 hover:text-foreground">
            <span class="flex-1 font-medium first-letter:uppercase">{{ name }}</span>
            <!-- Meme mot, meme couleur que le badge d'etat de ConfigView : la
                 cle `plugin_disconnected` n'existe pas dans le catalogue, et
                 `unavailable` est deja celle qui designe un greffon injoignable
                 la-bas — pas de doublon terminologique entre les deux pages. -->
            <Badge v-if="!connecte(name)" variant="destructive">{{ t('unavailable') }}</Badge>
            <ChevronRightIcon class="size-4 text-muted-foreground" />
          </RouterLink>
        </li>
      </ul>
      <p v-else class="text-sm text-muted-foreground" data-plugins-vide>{{ t('plugins_list_empty') }}</p>
    </CardContent>
  </Card>
</template>
