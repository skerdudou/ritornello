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
  <div v-if="texteEcoule" class="space-y-1" data-progression>
    <div v-if="barreVisible && deplacable" @keydown.capture="auClavier">
      <Slider
        data-barre
        :model-value="[affichee ?? 0]"
        :min="0"
        :max="duree ?? 0"
        :step="1"
        :aria-label="t('position_label')"
        @update:model-value="surChangement"
        @value-commit="surValidation"
      />
    </div>
    <div v-else-if="barreVisible" class="py-[19px]" data-barre>
      <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
        <div class="h-full rounded-full bg-primary" :style="{ width: pourcent + '%' }" data-remplissage />
      </div>
    </div>
    <div class="flex justify-between text-xs text-muted-foreground">
      <span data-position>{{ texteEcoule }}</span>
      <span v-if="texteDuree" data-duree-totale>{{ texteDuree }}</span>
    </div>
  </div>
</template>
