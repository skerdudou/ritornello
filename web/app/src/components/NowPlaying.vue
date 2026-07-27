<script setup lang="ts">
import { Badge, Card, CardContent } from '@ritornello/ui'
import { onMounted } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, riendAfficher, useNowPlaying } from '../composables/useNowPlaying'

const { t } = useCatalog()
const { etat, ouvre } = useNowPlaying()

onMounted(ouvre)
</script>

<template>
  <!--
    Rien de connu : rien affiche. Un cadre vide portant seulement « Morceau en
    cours » ferait croire a une panne, alors que la plupart des stations
    francaises n'annoncent tout simplement rien.
  -->
  <Card v-if="!riendAfficher(etat)" data-now-playing>
    <CardContent class="space-y-1 py-4">
      <div class="flex items-baseline gap-2">
        <p class="text-xs uppercase tracking-wide text-muted-foreground">{{ t('now_playing') }}</p>
        <!-- Qui a fourni l'information : c'est la premiere question qu'on se
             pose devant un titre faux. -->
        <Badge v-if="etat?.origin" variant="secondary" class="text-[10px]" data-origin>
          {{ etat.origin }}
        </Badge>
        <!--
          `title` explicite : « 3:34 » seul se lirait comme un temps ecoule sur
          un lecteur, alors que c'est la duree totale du morceau annoncee par la
          station.
        -->
        <span
          v-if="formateDuree(etat?.duration_s)"
          class="text-xs text-muted-foreground"
          :title="t('track_length')"
          data-duree
        >
          {{ formateDuree(etat?.duration_s) }}
        </span>
      </div>
      <p v-if="etat?.title" class="text-lg font-medium leading-tight" data-titre>{{ etat.title }}</p>
      <p v-if="etat?.artist" class="text-sm text-foreground" data-artiste>{{ etat.artist }}</p>
      <p v-if="etat?.album" class="text-sm text-muted-foreground" data-album>{{ etat.album }}</p>
    </CardContent>
  </Card>
</template>
