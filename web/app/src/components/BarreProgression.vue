<script setup lang="ts">
import { computed } from 'vue'
import { formatePosition } from '../composables/usePlayer'

// Composant local a la SPA plutot qu'element du kit : seule la carte Player
// s'en sert, et le kit est le contrat des pages de plugins.
const props = defineProps<{
  position: number | null
  duree: number | null
  /** Le contenu accepte un deplacement (`seekable` de la charge utile). */
  deplacable: boolean
  /** Pas du clavier, en secondes : le meme que celui des touches physiques. */
  pas: number
}>()
const emit = defineEmits<{ deplacer: [secondes: number] }>()

const texteEcoule = computed(() => formatePosition(props.position))
const texteDuree = computed(() => formatePosition(props.duree))
// Une barre sans fin n'apprend rien : sans duree connue, seul l'ecoule
// s'affiche.
const barreVisible = computed(() => props.duree != null && props.duree > 0)
const pourcent = computed(() => {
  if (!barreVisible.value || props.position == null) return 0
  return Math.min(100, Math.max(0, (props.position / (props.duree as number)) * 100))
})

function viser(e: MouseEvent): void {
  if (!props.deplacable || !barreVisible.value) return
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect()
  if (rect.width <= 0) return
  const ratio = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width))
  emit('deplacer', Math.round(ratio * (props.duree as number)))
}

function auClavier(e: KeyboardEvent): void {
  if (!props.deplacable || props.duree == null) return
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
  emit('deplacer', Math.min(props.duree, Math.max(0, cible)))
}
</script>

<template>
  <div v-if="texteEcoule" class="mt-2 space-y-1" data-progression>
    <div
      v-if="barreVisible"
      class="h-1.5 w-full rounded-full bg-muted"
      :class="deplacable ? 'cursor-pointer' : ''"
      data-barre
      :role="deplacable ? 'slider' : undefined"
      :tabindex="deplacable ? 0 : undefined"
      :aria-valuemin="deplacable ? 0 : undefined"
      :aria-valuemax="deplacable ? duree ?? undefined : undefined"
      :aria-valuenow="deplacable ? position ?? undefined : undefined"
      :aria-valuetext="deplacable ? texteEcoule : undefined"
      @click="viser"
      @keydown="auClavier"
    >
      <div class="h-full rounded-full bg-primary" :style="{ width: pourcent + '%' }" data-remplissage />
    </div>
    <div class="flex justify-between text-xs text-muted-foreground">
      <span data-position>{{ texteEcoule }}</span>
      <span v-if="texteDuree" data-duree-totale>{{ texteDuree }}</span>
    </div>
  </div>
</template>
