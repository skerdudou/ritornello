<script setup lang="ts">
import { Slider } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, formatePosition } from '../composables/usePlayer'

// Trois etats, et la charge utile les distingue tous :
//  - position inconnue : rien (une radio sans greffon de metadonnees) ;
//  - position connue mais pas `seekable` : une barre qui informe, sans
//    poignee ni role (Radio France annonce la duree d'un direct qu'on ne
//    rembobine pas) ;
//  - `seekable` : un vrai curseur, au doigt comme au clavier.
const { t } = useCatalog()
const props = defineProps<{
  position: number | null
  duree: number | null
  /** Le contenu accepte un deplacement (`seekable` de la charge utile). */
  deplacable: boolean
  /** Pas du clavier, en secondes : le meme que celui des touches physiques. */
  pas: number
}>()
const emit = defineEmits<{ deplacer: [secondes: number] }>()

// Valeur sous le doigt pendant le glisser ; null hors geste.
const locale = ref<number | null>(null)
// Valeur validee, en attente de la trame qui la confirme. Sans elle, la trame
// suivante — celle d'avant le saut, deja en route — ramenait la poignee en
// arriere un instant.
const visee = ref<number | null>(null)
watch(
  () => props.position,
  (p) => {
    if (visee.value === null) return
    // Trame sans position (fin de piste, Stop, veille, changement de source) :
    // rien ne viendra jamais confirmer le saut, la valeur visee doit se
    // relacher tout de suite plutot que de figer la barre sur une cible morte.
    if (p == null) {
      visee.value = null
      return
    }
    if (Math.abs(p - visee.value) <= props.pas) visee.value = null
  },
)
const affichee = computed(() => locale.value ?? visee.value ?? props.position)

const texteEcoule = computed(() => formatePosition(affichee.value))
// formateDuree, pas formatePosition : ce dernier accepte zero, alors qu'une
// duree totale de "0:00" n'en est pas une (voir sa doc dans usePlayer.ts).
const texteDuree = computed(() => formateDuree(props.duree))
// Une barre sans fin n'apprend rien : sans duree connue, seul l'ecoule s'affiche.
const barreVisible = computed(() => props.duree != null && props.duree > 0)
const pourcent = computed(() => {
  if (!barreVisible.value || affichee.value == null) return 0
  return Math.min(100, Math.max(0, (affichee.value / (props.duree as number)) * 100))
})

// `update:modelValue` de reka peut emettre `undefined` (cas d'une poignee
// retiree, hors de notre usage a une seule poignee) : le type le prevoit, pas
// notre logique — on retombe alors sur 0 sans planter.
function surChangement(v: number[] | undefined): void {
  locale.value = v?.[0] ?? 0
}

function surValidation(v: number[]): void {
  const s = Math.round(v[0] ?? 0)
  locale.value = null
  visee.value = s
  emit('deplacer', s)
}

// Le clavier reste au pas des touches physiques, pas a la seconde du curseur :
// capture sur l'enveloppe, avant le gestionnaire de reka sur la poignee.
function auClavier(e: KeyboardEvent): void {
  if (!props.deplacable || props.duree == null) return
  // Depuis la position confirmee (`position`), pas depuis `affichee` : la
  // valeur visee peut encore etre refusee par l'appareil, et partir d'elle
  // ferait deriver les pas suivants d'une hypothese non confirmee plutot que
  // de la verite connue.
  const depuis = props.position ?? 0
  const cible = {
    ArrowRight: depuis + props.pas,
    ArrowUp: depuis + props.pas,
    ArrowLeft: depuis - props.pas,
    ArrowDown: depuis - props.pas,
    Home: 0,
    End: props.duree,
  }[e.key]
  if (cible === undefined) return
  e.preventDefault()
  e.stopPropagation()
  surValidation([Math.min(props.duree, Math.max(0, cible))])
}
</script>

<template>
  <!--
    Demande du proprietaire : beaucoup trop d'air avant et apres la barre,
    les durees pourraient etre quasiment collees a la piste. `-mt-4`
    rapproche la barre du bloc texte au-dessus : le `gap-6` (24 px) du
    `Card` du kit qui separe les deux `CardContent` de `PlayerCard` n'est
    pas modifiable ici sans toucher un composant partage, donc c'est cote
    `BarreProgression` qu'on le compense — mesure a l'ecran (Playwright,
    390 px) apres un premier essai a `-mt-2` : il n'en rendait que 8 des
    24 px, insuffisant pour passer sous la cible de 12 px. Le `space-y-1`
    d'origine est retire au profit d'un `mt-0.5` porte par la seule ligne
    des durees, plus proche de ce qu'elle doit longer.
    `flex flex-col` sur la racine : un bloc ordinaire fusionne sa marge avec
    celle de son premier enfant (margin collapsing CSS) — ici l'enveloppe du
    curseur, dont le `-my-[19px]` du `Slider` remontait alors seul (le CSS ne
    garde que la plus negative des deux, -19px, au lieu de les additionner
    a -mt-4). Mesure a l'ecran : il restait 24 px au lieu de 8 sur le cas
    fichier. Un conteneur flex ne fusionne pas ses marges avec ses enfants,
    ce qui restaure l'addition attendue ; la barre statique, sans marge
    propre, n'est pas affectee.
  -->
  <div v-if="texteEcoule" class="-mt-4 flex flex-col" data-progression>
    <div v-if="barreVisible && deplacable" @keydown.capture="auClavier">
      <!--
        `-my-[19px]` : le curseur garde sa zone de contact de 44 px (le
        `py-[19px]` du kit, intouche) sans la faire payer a la mise en page.
        Mesure a l'ecran : `-my-3` (12 px) ne compensait que 12 des 19 px de
        padding, laissant encore 31 px entre le bloc texte et la piste sur
        le cas fichier. En reprenant l'integralite du padding, la boite
        visuelle du curseur redevient la piste de 6 px, comme la barre
        statique — la zone de contact deborde alors entierement sur ses
        voisins : la ligne des durees en dessous (du texte, jamais une
        cible, elle reste inerte au sens plein) et la ligne des badges
        au-dessus, qui elle N'EST PAS inerte quand elle porte des liens de
        plateformes (`data-lien`, des ancres de 44 px, cf. PlayerCard.vue) —
        le debordement de 19 px y recouvrirait leur bas et leur volerait le
        tap. C'est PlayerCard.vue qui les fait passer devant dans l'ordre de
        peinture (`relative z-10` sur `[data-liens]`) pour rendre le tap aux
        liens ; cote curseur, rien d'autre a faire ici. La poignee reste
        cliquable au bord : la marge negative deplace la boite, pas la zone
        de contact qu'elle contient (le padding, lui, ne bouge pas).
      -->
      <Slider
        data-barre
        class="-my-[19px]"
        :model-value="[affichee ?? 0]"
        :min="0"
        :max="duree ?? 0"
        :step="1"
        :aria-label="t('position_label')"
        @update:model-value="surChangement"
        @value-commit="surValidation"
      />
    </div>
    <!-- `py-0`, pas `py-[19px]` : cette barre n'est pas une cible (pas de
         `deplacable`), elle n'a donc pas a reserver une zone de contact —
         et `py-0` plutot que `py-1` pour que radio (barre statique) et
         fichier (curseur) partagent exactement la meme geometrie, la
         piste de 6 px collant directement a ses voisins dans les deux cas. -->
    <div v-else-if="barreVisible" class="py-0" data-barre>
      <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
        <div class="h-full rounded-full bg-primary" :style="{ width: pourcent + '%' }" data-remplissage />
      </div>
    </div>
    <div class="mt-0.5 flex justify-between text-xs text-muted-foreground">
      <span data-position>{{ texteEcoule }}</span>
      <span v-if="texteDuree" data-duree-totale>{{ texteDuree }}</span>
    </div>
  </div>
</template>
