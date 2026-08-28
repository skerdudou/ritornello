<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { Badge, Card, CardAction, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import ProgressBar from './ProgressBar.vue'
import ProvenanceDetails from './ProvenanceDetails.vue'
import AppleMusicIcon from './icons/AppleMusicIcon.vue'
import DeezerIcon from './icons/DeezerIcon.vue'
import YoutubeIcon from './icons/YoutubeIcon.vue'
import { LINK_LABEL } from './links'
import { useCatalog } from '../composables/useCatalog'
import { formatDuration, nothingToShow } from '../composables/usePlayer'
import type { PlayerPayload } from '../types'

// L'state vient du parent (HomeView), qui tient l'**unique** connexion SSE de
// la page : la telecommande en a besoin elle aussi (touche active), et ouvrir
// une seconde connexion ici doublerait les flux pour le meme contenu.
const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null; seekStep: number }>()
// L'appareil a annonce une pochette, le browser n'a step pu la charger.
//
// Le cas n'est step theorique : la cle du cache du coeur est bornee a quelques
// entries, et le fichier lui-meme vit sur un partage qui peut disparaitre —
// les deux rendent un 404 sous une URL deja publiee. Sans ce drapeau, le carre
// reserve montrait le glyphe d'image cassee du browser au lieu du repli ♫
// prevu pour exactement cette situation.
const imageBroken = ref(false)

/**
 * Nombre de reprises accordees a une image annoncee, avant le repli ♫.
 *
 * **Un echec n'est plus definitif pour la piste**, et c'est le point. Le
 * proprietaire rapporte des pochettes qui ne se chargent step, dont certaines
 * finissent par arriver « beaucoup plus tard » : la publication de l'URL et la
 * disponibilite reelle des octets ne sont step le meme instant, et le premier
 * `error` de l'`<img>` condamnait le carre jusqu'au morceau suivant. Deux
 * reprises espacees rattrapent un creux passager sans marteler l'appareil.
 */
const IMAGE_RETRIES = 2
/** Delai avant chaque reprise, en millisecondes : court, puis moins court. */
const RETRY_DELAYS_MS = [800, 3000]
/** Combien de reprises ont deja ete consommees pour l'URL courante. */
const retriesDone = ref(0)
/**
 * Compteur ajoute a l'URL des reprises.
 *
 * Sans lui le browser resservirait son propre echec mis en cache : une
 * reponse 404 est cachable, et redemander la meme URL ne repartirait step sur le
 * reseau. Il ne bouge qu'aux reprises, donc le cas nominal garde une URL stable
 * et le cache du browser joue son role.
 */
const attempt = ref(0)
let retryTimer: ReturnType<typeof setTimeout> | null = null

function cancelRetry() {
  if (retryTimer !== null) {
    clearTimeout(retryTimer)
    retryTimer = null
  }
}

/**
 * L'`<img>` a echoue : resume si le budget le permet, sinon replier.
 *
 * **Le repli ♫ est pose dans les deux cas**, tout de suite. Laisser l'`<img>`
 * en place pendant l'wait rendrait le glyphe d'image cassee du browser —
 * exactement ce que `imageBroken` existe pour eviter — et une reprise le
 * ferait clignoter. Le carre montre donc le repli, et l'image revient d'elle
 * meme si la reprise aboutit.
 */
function onImageError() {
  imageBroken.value = true
  if (retriesDone.value >= IMAGE_RETRIES) return
  const delai = RETRY_DELAYS_MS[retriesDone.value] ?? 3000
  retriesDone.value += 1
  cancelRetry()
  retryTimer = setTimeout(() => {
    retryTimer = null
    // L'order count : la nouvelle URL d'abord, le remontage ensuite. En
    // sens inverse, l'`<img>` reparaitrait un instant avec l'URL qui vient
    // d'fail, et le browser resservirait son echec en cache.
    attempt.value += 1
    imageBroken.value = false
  }, delai)
}

// Remis a zero des que l'appareil designe une **autre** image : sans cela, un
// seul echec condamnerait le carre pour le reste de la session.
watch(
  () => props.state?.cover_href,
  () => {
    imageBroken.value = false
    // Le budget de reprises est **par image** : une nouvelle URL repart avec
    // le sien, et la minuterie de la precedente n'a plus d'objet.
    retriesDone.value = 0
    attempt.value = 0
    cancelRetry()
    // Une pochette enlarged qui reste ouverte pendant que la piste change
    // montrerait l'image de la piste suivante en plein ecran, sans que
    // personne l'ait demande. Fermer est la seule reponse honnete.
    enlarged.value = false
  },
)
// Vrai quand l'appareil annonce une image et que le browser a su la charger :
// c'est la seule condition sous laquelle le carre est cliquable.
const hasImage = computed(() => !!props.state?.cover_href && !imageBroken.value)
/**
 * L'URL de la **vignette**, celle que le carre de la carte displayed.
 *
 * Le carre fait 224 px sur phone ; y charger le `folder.jpg` d'un NAS —
 * couramment deux ou trois mebioctets — etait du gaspillage pur, surtout en
 * Wi-Fi. Le coeur sait fabriquer la reduction (c'est celle qu'il pousse deja
 * aux afficheurs), il suffit de la lui demander. L'URL nue reste l'image telle
 * qu'elle est, et c'est elle que la vue enlarged load.
 */
const thumbnailHref = computed(() => {
  if (!props.state?.cover_href) return null
  const base = `${props.state.cover_href}?taille=vignette`
  // `attempt` n'apparait qu'a partir de la premiere reprise : voir sa doc.
  return attempt.value === 0 ? base : `${base}&attempt=${attempt.value}`
})
/** La pochette est-elle ouverte en plein ecran ? */
const enlarged = ref(false)
// Echap ferme, comme toute surcouche modale. L'ecouteur n'existe que pendant
// l'ouverture : un ecouteur global permanent pour une vue rarement ouverte est
// une dette, et il capterait des touches sur des pages qui n'ont step de
// pochette du tout.
function onEscape(e: KeyboardEvent) {
  if (e.key === 'Escape') enlarged.value = false
}
watch(enlarged, (ouverte) => {
  if (ouverte) window.addEventListener('keydown', onEscape)
  else window.removeEventListener('keydown', onEscape)
})
// Sans cela, quitter la page pochette ouverte laisse l'ecouteur derriere lui —
// et la minuterie de reprise tournerait contre un composant demonte.
onUnmounted(() => {
  window.removeEventListener('keydown', onEscape)
  cancelRetry()
})
// La duration ne s'displayed que faute de barre de progression : quand une position
// est connue, la barre porte deja la duration totale.
const durationToShow = computed(
  () => props.state?.position_s == null && !!formatDuration(props.state?.duration_s),
)
// Les links que cette version sait rendre. Le protocole ferme l'ensemble des
// plateformes, mais un greffon en avance sur l'IHM peut en nommer une nouvelle :
// la laisser passer donnerait une ancre de 44 px sans icon et sans nom
// accessible (`LINK_LABEL` n'aurait aucune entree pour elle). Filtrer ici
// plutot que de tenter un rendu par defaut, qui annoncerait « Ecouter sur Apple
// Music » pour un lien qui n'y mene step.
const links = computed(
  () => props.state?.links?.filter((lien) => lien.platform in LINK_LABEL) ?? [],
)
// La provenance a quelque chose a dire des que le coeur a nomme un champ ou un
// contributeur bredouille. C'est ce qui decide de la presence du `(?)`, donc de
// celle de la ligne quand rien d'autre ne l'occupe.
const hasOrigins = computed(() => {
  const p = props.state?.provenance
  return Object.keys(p?.fields ?? {}).length > 0 || (p?.misses?.length ?? 0) > 0
})
// La ligne basse du bloc morceau (provenance, duration, links) n'existe que s'il y
// a quelque chose a y mettre : sinon `min-h-11` reserverait 44 px vides sous
// l'album, ce qui est le cas le plus courant (un titre ICY nu).
const badgeRow = computed(
  () => hasOrigins.value || durationToShow.value || links.value.length > 0,
)
// Remonte au parent : c'est HomeView qui poste les commandes (comme pour le
// reste de la telecommande), la carte elle-meme n'en poste aucune.
const emit = defineEmits<{ seek: [secondes: number] }>()
</script>

<template>
  <!--
    La pochette et le morceau sont le sujet : c'est la seule chose qu'on
    regarde depuis le canape. L'state (source, veille) tient dans l'en-tete ;
    le volume est le curseur du slot `commandes`. Sur phone tout est
    centre en colonne ; a partir de `md` la pochette passe a gauche du text.
  -->
  <Card data-player>
    <CardHeader class="pb-2">
      <CardTitle class="flex items-center gap-2 text-base">
        {{ t('player_title') }}
        <!-- La source en pastille : un badge du kit, `data-source` conserve
             pour les journey. Le point dit « ca joue » (playback), la ou
             l'ancienne ligne de text ne disait rien.

             `bg-current` et non `bg-primary` : le point herite de la color
             de text du badge, donc il contraste **par construction** avec son
             propre fond, dans tous les themes. En `bg-primary` il peignait le
             vert du theme sur le bleu du badge secondaire — deux teintes
             saturees et proches, signalees illisibles par le proprietaire.
             C'est aussi l'idiome deja retenu pour la pastille de la
             preselection active (voir `PresetGrid.vue`). La color ne
             porte d'ailleurs aucun sens ici : c'est la **presence** du point
             qui dit que ca joue, il n'est rendu qu'a ce moment-la. -->
        <Badge variant="secondary" class="gap-1.5 font-normal">
          <span
            v-if="state?.playback === 'playing'"
            class="size-1.5 rounded-full bg-current"
            aria-hidden="true"
            data-lecture-en-cours
          />
          <span data-source>{{ state ? state.source || t('no_source') : '' }}</span>
        </Badge>
        <Badge v-if="state?.standby" variant="secondary" data-standby>{{ t('standby') }}</Badge>
      </CardTitle>
      <CardAction v-if="$slots.actions">
        <slot name="actions" />
      </CardAction>
    </CardHeader>
    <CardContent class="flex flex-col items-center gap-4 md:flex-row md:items-start md:gap-5">
      <!-- Le carre est toujours la, image ou repli : c'est lui qui tient la
           mise en page, et une image qui arrive apres le text ne doit rien
           decaler. 224 px sur phone (le sujet), 176 px a cote du text
           sur PC. -->
      <div
        class="size-56 shrink-0 overflow-hidden rounded-lg border border-border bg-muted shadow-md md:size-44"
        :class="{ 'opacity-50': state?.standby }"
        data-pochette
      >
        <!-- Un vrai bouton et non un `@click` sur l'image : la vue enlarged
             s'ouvre alors aussi au clavier et porte un nom accessible. Il n'y
             en a step quand il n'y a rien a agrandir — le repli ♫ n'est step une
             image, et un bouton qui n'ouvre rien est pire qu'aucun bouton. -->
        <button
          v-if="hasImage"
          type="button"
          class="size-full cursor-zoom-in focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
          :aria-label="t('cover_zoom')"
          :title="t('cover_zoom')"
          data-pochette-agrandir
          @click="enlarged = true"
        >
          <img
            :src="thumbnailHref!"
            :alt="t('cover_alt')"
            class="size-full object-cover"
            @error="onImageError"
          />
        </button>
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
             n'en declare step (cd sans disque, entree aux). -->
        <p v-if="state?.preset != null" class="text-[11px] font-semibold uppercase tracking-wider text-primary">
          P<span data-player-preset>{{ state.preset }}</span>
          <template v-if="state.preset_name"> · <span data-player-preset-name>{{ state.preset_name }}</span></template>
        </p>
        <!-- Le statut de la source (« PAS DE DISQUE »), masque en veille : le
             badge VEILLE porte deja le mot. -->
        <p v-if="state?.status && !state.standby" class="text-sm text-muted-foreground" data-player-status>
          {{ state.status }}
        </p>
        <div v-if="!nothingToShow(state)" class="flex min-w-0 flex-col items-center gap-0.5 md:items-start" data-now-playing>
          <p v-if="state?.title" class="text-xl font-semibold leading-tight text-foreground" data-titre>{{ state.title }}</p>
          <p v-if="state?.artist" class="text-sm text-foreground" data-artiste>{{ state.artist }}</p>
          <!-- L'annee s'accole a l'album, la ou une annee se lit. Elle sort
               aussi seule : un flux peut la connaitre sans connaitre l'album. -->
          <p v-if="state?.album || state?.year" class="text-sm text-muted-foreground">
            <span v-if="state?.album" data-album>{{ state.album }}</span>
            <span v-if="state?.album && state?.year"> · </span>
            <span v-if="state?.year" :title="t('release_year')" data-annee>{{ state.year }}</span>
          </p>
          <!-- Qui a fourni le text, et la pochette quand ce n'est step le meme :
               la premiere question devant un titre faux. Les plateformes
               d'ecoute partagent cette ligne : une rangee a elles seules
               poussait le curseur de volume hors de portee du pouce sur
               phone. `min-h-11` reserve d'emblee la hauteur de la cible
               tactile, sinon un lien qui arrive apres le titre (MusicBrainz
               repond plus tard) ferait grandir la carte sous le doigt.
               La ligne n'existe que s'il y a quelque chose a y mettre. -->
          <div
            v-if="badgeRow"
            class="mt-1 flex min-h-11 items-center gap-1.5"
            data-badges
          >
            <!-- Les deux badges d'origine ont cede la place a ce bouton
                 (decision du proprietaire) : ils occupaient la ligne la plus
                 chargee de l'ecran avec deux mots que personne ne lit en
                 ecoutant, et ils ne repondaient meme step a la question qu'on
                 se pose devant un titre faux — *quel champ* vient de *qui*.
                 Le detail vit desormais dans une popin, ou il y a la place de
                 le dire en toutes lettres. -->
            <ProvenanceDetails :state="state" />
            <span
              v-if="durationToShow"
              class="text-xs text-muted-foreground"
              :title="t('track_length')"
              data-duration
            >
              {{ formatDuration(state?.duration_s) }}
            </span>
            <!-- `platform` est un ensemble ferme cote protocole et l'URL a deja
                 ete validee contre l'hote de cette plateforme : rien a
                 revalider ici. `noopener` parce que la cible est un tiers,
                 `noreferrer` parce qu'il n'a step a savoir d'ou on vient.
                 La cle est l'URL et non la plateforme : rien n'interdit deux
                 links d'une meme plateforme, et Vue en perdrait un.
                 L'ancre ne porte plus de color elle-meme (ni au repos, ni au
                 survol) : chaque icon porte deja sa color de marque en dur
                 (decision du proprietaire, exception assumee a la regle
                 « aucune color en dur », voir docs/interface.md § Player
                 card), et une teinte de text par-dessus la brouillerait sans
                 rien apporter. `hover:opacity-80` garde un retour perceptible
                 au survol malgre l'absence de changement de color.
                 `relative z-10` : la zone de contact de 44 px du curseur de
                 ProgressBar deborde de 19 px au-dessus de sa piste (voir
                 ProgressBar.vue), alors que cette ligne n'est qu'a 8 px
                 plus haut — le debordement recouvre donc le bas de ces
                 ancres (des cibles reelles, contrairement aux durees en
                 dessous de la piste). Les faire passer devant dans l'order
                 de peinture rend le tap aux links : le curseur garde toute
                 sa zone de contact basse et au moins 33 px en haut, largement
                 assez pour rester utilisable. -->
            <span v-if="links.length" class="relative z-10 inline-flex items-center gap-1" data-links>
              <a
                v-for="lien in links"
                :key="lien.url"
                :href="lien.url"
                target="_blank"
                rel="noopener noreferrer"
                class="inline-flex size-11 items-center justify-center rounded-md transition-opacity hover:opacity-80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                :aria-label="t(LINK_LABEL[lien.platform])"
                :title="t(LINK_LABEL[lien.platform])"
                :data-lien="lien.platform"
              >
                <!-- Aucun `v-else` : les trois branches epuisent l'ensemble
                     deja filtre par `links`, et un `v-else` rendrait l'icon
                     Apple pour tout le reste. -->
                <YoutubeIcon v-if="lien.platform === 'youtube'" class="size-5" />
                <DeezerIcon v-else-if="lien.platform === 'deezer'" class="size-5" />
                <AppleMusicIcon v-else-if="lien.platform === 'apple_music'" class="size-5" />
              </a>
            </span>
          </div>
        </div>
      </div>
    </CardContent>
    <!-- L'entourage de la barre de progression a ete resserre a la demande du
         proprietaire, puis **desserre de 4 px** : a `space-y-2` la ligne des
         durees touchait les commandes, « colle au pixel pres ». `space-y-3`
         rend le tout petit ecart demande sans revenir a l'air d'avant. Le
         pendant au-dessus de la piste vit dans `ProgressBar.vue`
         (`-mt-3`), le `gap-6` du `Card` du kit n'etant step modifiable ici. -->
    <CardContent class="space-y-3 pt-0">
      <ProgressBar
        :position="state?.position_s ?? null"
        :duration="state?.duration_s ?? null"
        :seekable="state?.seekable ?? false"
        :step="seekStep"
        @seek="(s) => emit('seek', s)"
      />
      <slot name="commandes" />
    </CardContent>
    <!-- La pochette en plein ecran. `Teleport` vers le `body` : la carte a un
         `overflow-hidden` (arrondis) et son propre contexte d'empilement, une
         surcouche rendue dedans s'y serait retrouvee coupee. Un clic
         **n'importe ou** referme, y compris sur l'image : c'est la demande
         (« fermer en cliquant de nouveau »), et c'est aussi ce que fait tout
         visionneur d'images. -->
    <Teleport to="body">
      <div
        v-if="enlarged"
        class="fixed inset-0 z-50 flex cursor-zoom-out items-center justify-center bg-black/80 p-4"
        role="dialog"
        aria-modal="true"
        :aria-label="t('cover_alt')"
        data-pochette-enlarged
        @click="enlarged = false"
      >
        <!-- `object-contain` et non `object-cover` : agrandir sert justement a
             voir la pochette entiere, une rognure la trahirait. -->
        <img
          :src="state?.cover_href ?? ''"
          :alt="t('cover_alt')"
          class="max-h-full max-w-full rounded-lg object-contain shadow-2xl"
        />
        <!-- Le bouton de fermeture double le clic sur le fond, il ne le
             remplace step : sans lui, il n'existe aucun moyen de fermer au
             clavier autre qu'Echap, qui ne s'annonce nulle part. -->
        <button
          type="button"
          class="absolute right-4 top-4 rounded-full bg-black/50 p-3 text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white"
          :aria-label="t('cover_zoom_close')"
          :title="t('cover_zoom_close')"
          data-pochette-fermer
          @click.stop="enlarged = false"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </div>
    </Teleport>
  </Card>
</template>
