<script setup lang="ts">
import { Badge, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import BarreProgression from './BarreProgression.vue'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, riendAfficher } from '../composables/usePlayer'
import type { PlayerPayload } from '../types'

// L'etat vient du parent (HomeView), qui tient l'**unique** connexion SSE de
// la page : la telecommande en a besoin elle aussi (touche active), et ouvrir
// une seconde connexion ici doublerait les flux pour le meme contenu.
const { t } = useCatalog()
defineProps<{ etat: PlayerPayload | null; pasDeplacement: number }>()
// Remonte au parent : c'est HomeView qui poste les commandes (comme pour le
// reste de la telecommande), la carte elle-meme n'en poste aucune.
const emit = defineEmits<{ deplacer: [secondes: number] }>()
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
        <!-- Le muet n'est plus annoncé ici mais sur la ligne du volume, seule
             place où on le cherche. Deux mentions du même état seraient du
             bruit, et c'est celle du titre qui passait inaperçue. -->
      </CardTitle>
    </CardHeader>
    <CardContent class="space-y-1 pb-4">
      <!-- Depuis l'enregistrement a chaud, le coeur demarre sans source : un
           greffon lent peut s'annoncer bien apres, et la page doit etre la pour
           le montrer. La chaine vide **est** cette absence — le protocole ne
           change pas, c'est au rendu de la nommer, sinon on lit « Source
           active : » suivi de rien et on croit a une panne d'affichage.

           Le ternaire distingue les deux vides : `etat` a `null`, c'est
           « l'etat n'est pas encore arrive » (avant la premiere trame SSE), et
           annoncer « Aucune source » a ce moment-la serait faux. Meme idiome
           que la ligne du volume juste en dessous. -->
      <p class="text-sm text-muted-foreground">
        {{ t('active_source_label') }} :
        <span class="text-foreground" data-source>{{
          etat ? etat.source || t('no_source') : ''
        }}</span>
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
           n'existait que dans une ligne d'afficheur.

           Masqué en veille : le badge "VEILLE" juste au-dessus porte déjà ce
           mot, et le statut publié en veille est exactement le même mot du
           même catalogue — l'afficher aussi ici doublerait "VEILLE" sur la
           carte, sans libellé la seconde fois. -->
      <p v-if="etat?.status && !etat.standby" class="text-sm text-muted-foreground">
        <span class="text-foreground" data-player-status>{{ etat.status }}</span>
      </p>
      <!-- La sourdine se dit **ici**, sur la ligne du volume, et non dans le
           titre de l'encart : signalé à l'usage, on lisait « Volume : 60 % »
           sans remarquer le badge deux lignes plus haut, donc sans comprendre
           pourquoi rien ne sortait. La valeur est barrée plutôt que masquée —
           elle reste vraie, et c'est celle qui revient au rétablissement. -->
      <p class="flex items-center gap-2 text-sm text-muted-foreground" data-volume-ligne>
        <span>
          {{ t('volume') }} :
          <span
            class="text-foreground"
            :class="{ 'line-through opacity-60': etat?.muted }"
            data-volume
            >{{ etat ? etat.volume + ' %' : '' }}</span
          >
        </span>
        <Badge v-if="etat?.muted" variant="secondary" data-muted>{{ t('muted') }}</Badge>
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
          <!-- Seulement quand la position n'est pas connue : sinon la barre
               juste en dessous affiche deja "ecoule ... duree", et repeter la
               duree seule ici serait la meme information deux fois (defaut
               corrige : "4:14" dans l'en-tete, "1:27 ... 4:14" dans la barre). -->
          <span
            v-if="etat?.position_s == null && formateDuree(etat?.duration_s)"
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

      <!-- Hors du bloc « en ecoute » ci-dessus, et c'est un defaut corrige :
           ce bloc est garde par la presence de metadonnees, si bien que la
           barre disparaissait sur un fichier sans etiquettes ou un disque non
           reconnu — precisement les cas ou mpv connait le mieux la position.
           Savoir ou l'on en est ne depend pas d'avoir un titre. -->
      <BarreProgression
        :position="etat?.position_s ?? null"
        :duree="etat?.duration_s ?? null"
        :deplacable="etat?.seekable ?? false"
        :pas="pasDeplacement"
        @deplacer="(s) => emit('deplacer', s)"
      />
    </CardContent>
  </Card>
</template>
