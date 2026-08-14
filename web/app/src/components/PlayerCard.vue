<script setup lang="ts">
import { Badge, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, riendAfficher } from '../composables/usePlayer'
import type { PlayerPayload } from '../types'

// L'etat vient du parent (HomeView), qui tient l'**unique** connexion SSE de
// la page : la telecommande en a besoin elle aussi (touche active), et ouvrir
// une seconde connexion ici doublerait les flux pour le meme contenu.
const { t } = useCatalog()
defineProps<{ etat: PlayerPayload | null }>()
</script>

<template>
  <!--
    L'encart est toujours la : l'etat du lecteur (source, volume) est toujours
    connu. Le morceau, lui, s'y ajoute quand on le connait — la plupart des
    stations francaises n'annoncent rien, et un bloc « En ecoute » vide ferait
    croire a une panne.
  -->
  <Card data-player>
    <CardHeader class="pb-2">
      <CardTitle class="flex items-center gap-2 text-base">
        {{ t('player_title') }}
        <Badge v-if="etat?.standby" variant="secondary" data-standby>{{ t('standby') }}</Badge>
        <Badge v-if="etat?.muted" variant="secondary" data-muted>{{ t('muted') }}</Badge>
      </CardTitle>
    </CardHeader>
    <CardContent class="space-y-1 pb-4">
      <p class="text-sm text-muted-foreground">
        {{ t('active_source_label') }} :
        <span class="text-foreground" data-source>{{ etat?.source }}</span>
      </p>
      <!-- La touche numérotée de ce qui joue, quand la Source en déclare une.
           Absente plutôt qu'affichée vide : `null` signifie « rien ne joue, ou
           la Source ne numérote pas » (un cd sans disque, une entrée auxiliaire),
           et une ligne « Présélection : — » laisserait croire à une panne là où
           il n'y a simplement rien à numéroter. Même règle que le bloc « En
           écoute » juste en dessous.

           Le nom se colle au numéro dans la même ligne (data-player-preset
           reste sur le seul numéro, data-player-preset-name porte le nom) :
           aucune clé i18n dédiée, pour ne pas dire « station » là où ce n'en
           est pas toujours une — le cd, par exemple, ne déclare aucun nom. -->
      <p v-if="etat?.preset != null" class="text-sm text-muted-foreground">
        {{ t('player_preset') }} :
        <span class="text-foreground" data-player-preset>{{ etat.preset }}</span>
        <template v-if="etat.preset_name">
          — <span class="text-foreground" data-player-preset-name>{{ etat.preset_name }}</span>
        </template>
      </p>
      <!-- Le statut de la source, déjà traduit par elle. Invisible sur le web
           jusqu'ici pour la même raison que le nom de station l'était : il
           n'existait que dans une ligne d'afficheur. -->
      <p v-if="etat?.status" class="text-sm text-muted-foreground">
        <span class="text-foreground" data-player-status>{{ etat.status }}</span>
      </p>
      <p class="text-sm text-muted-foreground">
        {{ t('volume') }} :
        <span class="text-foreground" data-volume>{{ etat ? etat.volume + ' %' : '' }}</span>
      </p>

      <!-- Le morceau, quand il est connu. -->
      <div v-if="!riendAfficher(etat)" class="mt-3 border-t border-border pt-3" data-now-playing>
        <div class="flex items-baseline gap-2">
          <p class="text-xs uppercase tracking-wide text-muted-foreground">{{ t('now_playing') }}</p>
          <!-- Qui a fourni l'information : c'est la premiere question qu'on se
               pose devant un titre faux. -->
          <Badge v-if="etat?.origin" variant="secondary" class="text-[10px]" data-origin>
            {{ etat.origin }}
          </Badge>
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
      </div>
    </CardContent>
  </Card>
</template>
