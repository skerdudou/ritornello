<script setup lang="ts">
import { ref, watch } from 'vue'
import { Badge, Card, CardAction, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import BarreProgression from './BarreProgression.vue'
import IconeAppleMusic from './icones/IconeAppleMusic.vue'
import IconeDeezer from './icones/IconeDeezer.vue'
import IconeYoutube from './icones/IconeYoutube.vue'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, riendAfficher } from '../composables/usePlayer'
import type { PlayerPayload } from '../types'

// La cle de catalogue par plateforme. Une table, et non une cle fabriquee par
// concatenation a la volee : `i18nKeysUsed.test.ts` releve les cles citees
// litteralement dans le source, et une cle composee lui echapperait — elle
// pourrait disparaitre du catalogue sans qu'aucun test ne s'en apercoive.
//
// Ce commentaire a d'ailleurs fait tomber ce test en le citant : le prefixe
// ecrit entre guillemets etait lu comme une cle a part entiere. D'ou la
// paraphrase plutot qu'un exemple.
const LIBELLE_LIEN = {
  youtube: 'listen_on_youtube',
  deezer: 'listen_on_deezer',
  apple_music: 'listen_on_apple_music',
} as const

// L'etat vient du parent (HomeView), qui tient l'**unique** connexion SSE de
// la page : la telecommande en a besoin elle aussi (touche active), et ouvrir
// une seconde connexion ici doublerait les flux pour le meme contenu.
const { t } = useCatalog()
const props = defineProps<{ etat: PlayerPayload | null; pasDeplacement: number }>()
// L'appareil a annonce une pochette, le navigateur n'a pas pu la charger.
//
// Le cas n'est pas theorique : la cle du cache du coeur est bornee a quelques
// entrees, et le fichier lui-meme vit sur un partage qui peut disparaitre —
// les deux rendent un 404 sous une URL deja publiee. Sans ce drapeau, le carre
// reserve montrait le glyphe d'image cassee du navigateur au lieu du repli ♫
// prevu pour exactement cette situation.
const imageCassee = ref(false)
// Remis a zero des que l'appareil designe une **autre** image : sans cela, un
// seul echec condamnerait le carre pour le reste de la session.
watch(
  () => props.etat?.cover_href,
  () => {
    imageCassee.value = false
  },
)
// Remonte au parent : c'est HomeView qui poste les commandes (comme pour le
// reste de la telecommande), la carte elle-meme n'en poste aucune.
const emit = defineEmits<{ deplacer: [secondes: number] }>()
</script>

<template>
  <!--
    La pochette et le morceau sont le sujet : c'est la seule chose qu'on
    regarde depuis le canape. L'etat (source, veille) tient dans l'en-tete ;
    le volume est le curseur du slot `commandes`. Sur telephone tout est
    centre en colonne ; a partir de `md` la pochette passe a gauche du texte.
  -->
  <Card data-player>
    <CardHeader class="pb-2">
      <CardTitle class="flex items-center gap-2 text-base">
        {{ t('player_title') }}
        <!-- La source en pastille : un badge du kit, `data-source` conserve
             pour les parcours. Le point vert dit « ca joue » (playback), la
             ou l'ancienne ligne de texte ne disait rien. -->
        <Badge variant="secondary" class="gap-1.5 font-normal">
          <span
            v-if="etat?.playback === 'playing'"
            class="size-1.5 rounded-full bg-primary"
            aria-hidden="true"
            data-lecture-en-cours
          />
          <span data-source>{{ etat ? etat.source || t('no_source') : '' }}</span>
        </Badge>
        <Badge v-if="etat?.standby" variant="secondary" data-standby>{{ t('standby') }}</Badge>
      </CardTitle>
      <CardAction v-if="$slots.actions">
        <slot name="actions" />
      </CardAction>
    </CardHeader>
    <CardContent class="flex flex-col items-center gap-4 md:flex-row md:items-start md:gap-5">
      <!-- Le carre est toujours la, image ou repli : c'est lui qui tient la
           mise en page, et une image qui arrive apres le texte ne doit rien
           decaler. 224 px sur telephone (le sujet), 176 px a cote du texte
           sur PC. -->
      <div
        class="size-56 shrink-0 overflow-hidden rounded-lg border border-border bg-muted shadow-md md:size-44"
        :class="{ 'opacity-50': etat?.standby }"
        data-pochette
      >
        <img
          v-if="etat?.cover_href && !imageCassee"
          :src="etat.cover_href"
          :alt="t('cover_alt')"
          class="size-full object-cover"
          @error="imageCassee = true"
        />
        <div
          v-else
          class="flex size-full items-center justify-center text-muted-foreground"
          data-pochette-repli
          aria-hidden="true"
        >
          <svg width="56" height="56" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M9 18V5l12-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="18" cy="16" r="3" />
          </svg>
        </div>
      </div>
      <div class="flex min-w-0 flex-1 flex-col items-center gap-1 text-center md:items-start md:text-left">
        <!-- La presélection en surligne : `P1 · FIP`. Absente quand la source
             n'en declare pas (cd sans disque, entree aux). -->
        <p v-if="etat?.preset != null" class="text-[11px] font-semibold uppercase tracking-wider text-primary">
          P<span data-player-preset>{{ etat.preset }}</span>
          <template v-if="etat.preset_name"> · <span data-player-preset-name>{{ etat.preset_name }}</span></template>
        </p>
        <!-- Le statut de la source (« PAS DE DISQUE »), masque en veille : le
             badge VEILLE porte deja le mot. -->
        <p v-if="etat?.status && !etat.standby" class="text-sm text-muted-foreground" data-player-status>
          {{ etat.status }}
        </p>
        <div v-if="!riendAfficher(etat)" class="flex min-w-0 flex-col items-center gap-0.5 md:items-start" data-now-playing>
          <p v-if="etat?.title" class="text-xl font-semibold leading-tight text-foreground" data-titre>{{ etat.title }}</p>
          <p v-if="etat?.artist" class="text-sm text-foreground" data-artiste>{{ etat.artist }}</p>
          <!-- L'annee s'accole a l'album, la ou une annee se lit. Elle sort
               aussi seule : un flux peut la connaitre sans connaitre l'album. -->
          <p v-if="etat?.album || etat?.year" class="text-sm text-muted-foreground">
            <span v-if="etat?.album" data-album>{{ etat.album }}</span>
            <span v-if="etat?.album && etat?.year"> · </span>
            <span v-if="etat?.year" :title="t('release_year')" data-annee>{{ etat.year }}</span>
          </p>
          <!-- Les plateformes d'ecoute. `platform` est un ensemble ferme cote
               protocole et l'URL a deja ete validee contre l'hote de cette
               plateforme : rien a revalider ici. `noopener` parce que la cible
               est un tiers, `noreferrer` parce qu'il n'a pas a savoir d'ou on
               vient. -->
          <div v-if="etat?.links?.length" class="mt-1 flex items-center gap-2" data-liens>
            <a
              v-for="lien in etat.links"
              :key="lien.platform"
              :href="lien.url"
              target="_blank"
              rel="noopener noreferrer"
              class="rounded text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
              :aria-label="t(LIBELLE_LIEN[lien.platform])"
              :title="t(LIBELLE_LIEN[lien.platform])"
              :data-lien="lien.platform"
            >
              <IconeYoutube v-if="lien.platform === 'youtube'" />
              <IconeDeezer v-else-if="lien.platform === 'deezer'" />
              <IconeAppleMusic v-else />
            </a>
          </div>
          <!-- Qui a fourni le texte, et la pochette quand ce n'est pas le meme :
               la premiere question devant un titre faux. -->
          <div class="mt-1 flex items-center gap-1.5">
            <Badge v-if="etat?.origin" variant="secondary" class="text-[10px]" data-origin>{{ etat.origin }}</Badge>
            <Badge
              v-if="etat?.cover_origin && etat.cover_origin !== etat.origin"
              variant="secondary"
              class="text-[10px]"
              data-cover-origin
            >
              {{ etat.cover_origin }}
            </Badge>
            <span
              v-if="etat?.position_s == null && formateDuree(etat?.duration_s)"
              class="text-xs text-muted-foreground"
              :title="t('track_length')"
              data-duree
            >
              {{ formateDuree(etat?.duration_s) }}
            </span>
          </div>
        </div>
      </div>
    </CardContent>
    <CardContent class="space-y-3 pt-0">
      <BarreProgression
        :position="etat?.position_s ?? null"
        :duree="etat?.duration_s ?? null"
        :deplacable="etat?.seekable ?? false"
        :pas="pasDeplacement"
        @deplacer="(s) => emit('deplacer', s)"
      />
      <slot name="commandes" />
    </CardContent>
  </Card>
</template>
