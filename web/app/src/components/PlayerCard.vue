<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { Badge, Card, CardAction, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import BarreProgression from './BarreProgression.vue'
import DetailProvenance from './DetailProvenance.vue'
import IconeAppleMusic from './icones/IconeAppleMusic.vue'
import IconeDeezer from './icones/IconeDeezer.vue'
import IconeYoutube from './icones/IconeYoutube.vue'
import { LIBELLE_LIEN } from './liens'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, riendAfficher } from '../composables/usePlayer'
import type { PlayerPayload } from '../types'

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

/**
 * Nombre de reprises accordees a une image annoncee, avant le repli ♫.
 *
 * **Un echec n'est plus definitif pour la piste**, et c'est le point. Le
 * proprietaire rapporte des pochettes qui ne se chargent pas, dont certaines
 * finissent par arriver « beaucoup plus tard » : la publication de l'URL et la
 * disponibilite reelle des octets ne sont pas le meme instant, et le premier
 * `error` de l'`<img>` condamnait le carre jusqu'au morceau suivant. Deux
 * reprises espacees rattrapent un creux passager sans marteler l'appareil.
 */
const REPRISES_IMAGE = 2
/** Delai avant chaque reprise, en millisecondes : court, puis moins court. */
const DELAIS_REPRISE_MS = [800, 3000]
/** Combien de reprises ont deja ete consommees pour l'URL courante. */
const reprisesFaites = ref(0)
/**
 * Compteur ajoute a l'URL des reprises.
 *
 * Sans lui le navigateur resservirait son propre echec mis en cache : une
 * reponse 404 est cachable, et redemander la meme URL ne repartirait pas sur le
 * reseau. Il ne bouge qu'aux reprises, donc le cas nominal garde une URL stable
 * et le cache du navigateur joue son role.
 */
const essai = ref(0)
let minuterieReprise: ReturnType<typeof setTimeout> | null = null

function annuleReprise() {
  if (minuterieReprise !== null) {
    clearTimeout(minuterieReprise)
    minuterieReprise = null
  }
}

/**
 * L'`<img>` a echoue : reprendre si le budget le permet, sinon replier.
 *
 * **Le repli ♫ est pose dans les deux cas**, tout de suite. Laisser l'`<img>`
 * en place pendant l'attente rendrait le glyphe d'image cassee du navigateur —
 * exactement ce que `imageCassee` existe pour eviter — et une reprise le
 * ferait clignoter. Le carre montre donc le repli, et l'image revient d'elle
 * meme si la reprise aboutit.
 */
function surEchecImage() {
  imageCassee.value = true
  if (reprisesFaites.value >= REPRISES_IMAGE) return
  const delai = DELAIS_REPRISE_MS[reprisesFaites.value] ?? 3000
  reprisesFaites.value += 1
  annuleReprise()
  minuterieReprise = setTimeout(() => {
    minuterieReprise = null
    // L'ordre compte : la nouvelle URL d'abord, le remontage ensuite. En
    // sens inverse, l'`<img>` reparaitrait un instant avec l'URL qui vient
    // d'echouer, et le navigateur resservirait son echec en cache.
    essai.value += 1
    imageCassee.value = false
  }, delai)
}

// Remis a zero des que l'appareil designe une **autre** image : sans cela, un
// seul echec condamnerait le carre pour le reste de la session.
watch(
  () => props.etat?.cover_href,
  () => {
    imageCassee.value = false
    // Le budget de reprises est **par image** : une nouvelle URL repart avec
    // le sien, et la minuterie de la precedente n'a plus d'objet.
    reprisesFaites.value = 0
    essai.value = 0
    annuleReprise()
    // Une pochette agrandie qui reste ouverte pendant que la piste change
    // montrerait l'image de la piste suivante en plein ecran, sans que
    // personne l'ait demande. Fermer est la seule reponse honnete.
    agrandie.value = false
  },
)
// Vrai quand l'appareil annonce une image et que le navigateur a su la charger :
// c'est la seule condition sous laquelle le carre est cliquable.
const aUneImage = computed(() => !!props.etat?.cover_href && !imageCassee.value)
/**
 * L'URL de la **vignette**, celle que le carre de la carte affiche.
 *
 * Le carre fait 224 px sur telephone ; y charger le `folder.jpg` d'un NAS —
 * couramment deux ou trois mebioctets — etait du gaspillage pur, surtout en
 * Wi-Fi. Le coeur sait fabriquer la reduction (c'est celle qu'il pousse deja
 * aux afficheurs), il suffit de la lui demander. L'URL nue reste l'image telle
 * qu'elle est, et c'est elle que la vue agrandie charge.
 */
const vignetteHref = computed(() => {
  if (!props.etat?.cover_href) return null
  const base = `${props.etat.cover_href}?taille=vignette`
  // `essai` n'apparait qu'a partir de la premiere reprise : voir sa doc.
  return essai.value === 0 ? base : `${base}&essai=${essai.value}`
})
/** La pochette est-elle ouverte en plein ecran ? */
const agrandie = ref(false)
// Echap ferme, comme toute surcouche modale. L'ecouteur n'existe que pendant
// l'ouverture : un ecouteur global permanent pour une vue rarement ouverte est
// une dette, et il capterait des touches sur des pages qui n'ont pas de
// pochette du tout.
function surEchap(e: KeyboardEvent) {
  if (e.key === 'Escape') agrandie.value = false
}
watch(agrandie, (ouverte) => {
  if (ouverte) window.addEventListener('keydown', surEchap)
  else window.removeEventListener('keydown', surEchap)
})
// Sans cela, quitter la page pochette ouverte laisse l'ecouteur derriere lui —
// et la minuterie de reprise tournerait contre un composant demonte.
onUnmounted(() => {
  window.removeEventListener('keydown', surEchap)
  annuleReprise()
})
// La duree ne s'affiche que faute de barre de progression : quand une position
// est connue, la barre porte deja la duree totale.
const dureeAAfficher = computed(
  () => props.etat?.position_s == null && !!formateDuree(props.etat?.duration_s),
)
// Les liens que cette version sait rendre. Le protocole ferme l'ensemble des
// plateformes, mais un greffon en avance sur l'IHM peut en nommer une nouvelle :
// la laisser passer donnerait une ancre de 44 px sans icone et sans nom
// accessible (`LIBELLE_LIEN` n'aurait aucune entree pour elle). Filtrer ici
// plutot que de tenter un rendu par defaut, qui annoncerait « Ecouter sur Apple
// Music » pour un lien qui n'y mene pas.
const liens = computed(
  () => props.etat?.links?.filter((lien) => lien.platform in LIBELLE_LIEN) ?? [],
)
// La provenance a quelque chose a dire des que le coeur a nomme un champ ou un
// contributeur bredouille. C'est ce qui decide de la presence du `(?)`, donc de
// celle de la ligne quand rien d'autre ne l'occupe.
const aUneProvenance = computed(() => {
  const p = props.etat?.provenance
  return Object.keys(p?.fields ?? {}).length > 0 || (p?.misses?.length ?? 0) > 0
})
// La ligne basse du bloc morceau (provenance, duree, liens) n'existe que s'il y
// a quelque chose a y mettre : sinon `min-h-11` reserverait 44 px vides sous
// l'album, ce qui est le cas le plus courant (un titre ICY nu).
const ligneBadges = computed(
  () => aUneProvenance.value || dureeAAfficher.value || liens.value.length > 0,
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
             pour les parcours. Le point dit « ca joue » (playback), la ou
             l'ancienne ligne de texte ne disait rien.

             `bg-current` et non `bg-primary` : le point herite de la couleur
             de texte du badge, donc il contraste **par construction** avec son
             propre fond, dans tous les themes. En `bg-primary` il peignait le
             vert du theme sur le bleu du badge secondaire — deux teintes
             saturees et proches, signalees illisibles par le proprietaire.
             C'est aussi l'idiome deja retenu pour la pastille de la
             preselection active (voir `GrillePresets.vue`). La couleur ne
             porte d'ailleurs aucun sens ici : c'est la **presence** du point
             qui dit que ca joue, il n'est rendu qu'a ce moment-la. -->
        <Badge variant="secondary" class="gap-1.5 font-normal">
          <span
            v-if="etat?.playback === 'playing'"
            class="size-1.5 rounded-full bg-current"
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
        <!-- Un vrai bouton et non un `@click` sur l'image : la vue agrandie
             s'ouvre alors aussi au clavier et porte un nom accessible. Il n'y
             en a pas quand il n'y a rien a agrandir — le repli ♫ n'est pas une
             image, et un bouton qui n'ouvre rien est pire qu'aucun bouton. -->
        <button
          v-if="aUneImage"
          type="button"
          class="size-full cursor-zoom-in focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
          :aria-label="t('cover_zoom')"
          :title="t('cover_zoom')"
          data-pochette-agrandir
          @click="agrandie = true"
        >
          <img
            :src="vignetteHref!"
            :alt="t('cover_alt')"
            class="size-full object-cover"
            @error="surEchecImage"
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
          <!-- Qui a fourni le texte, et la pochette quand ce n'est pas le meme :
               la premiere question devant un titre faux. Les plateformes
               d'ecoute partagent cette ligne : une rangee a elles seules
               poussait le curseur de volume hors de portee du pouce sur
               telephone. `min-h-11` reserve d'emblee la hauteur de la cible
               tactile, sinon un lien qui arrive apres le titre (MusicBrainz
               repond plus tard) ferait grandir la carte sous le doigt.
               La ligne n'existe que s'il y a quelque chose a y mettre. -->
          <div
            v-if="ligneBadges"
            class="mt-1 flex min-h-11 items-center gap-1.5"
            data-badges
          >
            <!-- Les deux badges d'origine ont cede la place a ce bouton
                 (decision du proprietaire) : ils occupaient la ligne la plus
                 chargee de l'ecran avec deux mots que personne ne lit en
                 ecoutant, et ils ne repondaient meme pas a la question qu'on
                 se pose devant un titre faux — *quel champ* vient de *qui*.
                 Le detail vit desormais dans une popin, ou il y a la place de
                 le dire en toutes lettres. -->
            <DetailProvenance :etat="etat" />
            <span
              v-if="dureeAAfficher"
              class="text-xs text-muted-foreground"
              :title="t('track_length')"
              data-duree
            >
              {{ formateDuree(etat?.duration_s) }}
            </span>
            <!-- `platform` est un ensemble ferme cote protocole et l'URL a deja
                 ete validee contre l'hote de cette plateforme : rien a
                 revalider ici. `noopener` parce que la cible est un tiers,
                 `noreferrer` parce qu'il n'a pas a savoir d'ou on vient.
                 La cle est l'URL et non la plateforme : rien n'interdit deux
                 liens d'une meme plateforme, et Vue en perdrait un.
                 L'ancre ne porte plus de couleur elle-meme (ni au repos, ni au
                 survol) : chaque icone porte deja sa couleur de marque en dur
                 (decision du proprietaire, exception assumee a la regle
                 « aucune couleur en dur », voir docs/interface.md § Player
                 card), et une teinte de texte par-dessus la brouillerait sans
                 rien apporter. `hover:opacity-80` garde un retour perceptible
                 au survol malgre l'absence de changement de couleur.
                 `relative z-10` : la zone de contact de 44 px du curseur de
                 BarreProgression deborde de 19 px au-dessus de sa piste (voir
                 BarreProgression.vue), alors que cette ligne n'est qu'a 8 px
                 plus haut — le debordement recouvre donc le bas de ces
                 ancres (des cibles reelles, contrairement aux durees en
                 dessous de la piste). Les faire passer devant dans l'ordre
                 de peinture rend le tap aux liens : le curseur garde toute
                 sa zone de contact basse et au moins 33 px en haut, largement
                 assez pour rester utilisable. -->
            <span v-if="liens.length" class="relative z-10 inline-flex items-center gap-1" data-liens>
              <a
                v-for="lien in liens"
                :key="lien.url"
                :href="lien.url"
                target="_blank"
                rel="noopener noreferrer"
                class="inline-flex size-11 items-center justify-center rounded-md transition-opacity hover:opacity-80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                :aria-label="t(LIBELLE_LIEN[lien.platform])"
                :title="t(LIBELLE_LIEN[lien.platform])"
                :data-lien="lien.platform"
              >
                <!-- Aucun `v-else` : les trois branches epuisent l'ensemble
                     deja filtre par `liens`, et un `v-else` rendrait l'icone
                     Apple pour tout le reste. -->
                <IconeYoutube v-if="lien.platform === 'youtube'" class="size-5" />
                <IconeDeezer v-else-if="lien.platform === 'deezer'" class="size-5" />
                <IconeAppleMusic v-else-if="lien.platform === 'apple_music'" class="size-5" />
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
         pendant au-dessus de la piste vit dans `BarreProgression.vue`
         (`-mt-3`), le `gap-6` du `Card` du kit n'etant pas modifiable ici. -->
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
    <!-- La pochette en plein ecran. `Teleport` vers le `body` : la carte a un
         `overflow-hidden` (arrondis) et son propre contexte d'empilement, une
         surcouche rendue dedans s'y serait retrouvee coupee. Un clic
         **n'importe ou** referme, y compris sur l'image : c'est la demande
         (« fermer en cliquant de nouveau »), et c'est aussi ce que fait tout
         visionneur d'images. -->
    <Teleport to="body">
      <div
        v-if="agrandie"
        class="fixed inset-0 z-50 flex cursor-zoom-out items-center justify-center bg-black/80 p-4"
        role="dialog"
        aria-modal="true"
        :aria-label="t('cover_alt')"
        data-pochette-agrandie
        @click="agrandie = false"
      >
        <!-- `object-contain` et non `object-cover` : agrandir sert justement a
             voir la pochette entiere, une rognure la trahirait. -->
        <img
          :src="etat?.cover_href ?? ''"
          :alt="t('cover_alt')"
          class="max-h-full max-w-full rounded-lg object-contain shadow-2xl"
        />
        <!-- Le bouton de fermeture double le clic sur le fond, il ne le
             remplace pas : sans lui, il n'existe aucun moyen de fermer au
             clavier autre qu'Echap, qui ne s'annonce nulle part. -->
        <button
          type="button"
          class="absolute right-4 top-4 rounded-full bg-black/50 p-3 text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white"
          :aria-label="t('cover_zoom_close')"
          :title="t('cover_zoom_close')"
          data-pochette-fermer
          @click.stop="agrandie = false"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </div>
    </Teleport>
  </Card>
</template>
