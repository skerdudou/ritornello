<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ChevronLeftIcon, ChevronRightIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'
import { indisponible } from '../views/remoteCommands'

const { t } = useCatalog()
const props = defineProps<{ etat: PlayerPayload | null; nomDe: (n: number) => string | null }>()
const emit = defineEmits<{ choisir: [n: number] }>()

const page = ref(0)

// Compte déclaré par la source (null = source muette sur le sujet : grille
// 1-9 historique, pour ne jamais désarmer la télécommande).
const compte = computed(() => props.etat?.preset_count ?? null)

// Numéros de la page courante, seulement ceux qui existent. Page 0 : 1-9 (les
// touches nues de la télécommande) ; page k : 10k à 10k+9 (le 0 de la
// télécommande donne 10k). Mêmes bornes que le `+10` du cœur — ne pas
// « simplifier » en fenêtres de 6 ou 10 : la page web et la touche physique
// doivent désigner les mêmes groupes.
const presets = computed(() => {
  const c = compte.value
  if (c === null) return Array.from({ length: 9 }, (_, i) => i + 1)
  const debut = page.value === 0 ? 1 : page.value * 10
  const fin = Math.min(page.value * 10 + 9, c)
  return debut > fin ? [] : Array.from({ length: fin - debut + 1 }, (_, i) => debut + i)
})

const paginationVisible = computed(() => (compte.value ?? 0) > 9)

// Dernière page non vide : le plus grand multiple de 10 encore atteignable
// (même borne que le rebouclage du +10 côté cœur), 0 si tout tient sur 1-9.
const dernierePage = computed(() => {
  const c = compte.value ?? 0
  return c > 9 ? Math.floor(c / 10) : 0
})

const fenetre = computed(() => {
  const p = presets.value
  return p.length ? `${p[0]}–${p[p.length - 1]}` : ''
})

function pagePrecedente() {
  if (page.value > 0) page.value -= 1
}
function pageSuivante() {
  if (page.value < dernierePage.value) page.value += 1
}

const presetActif = computed(() => props.etat?.preset ?? null)

function pageDe(n: number) {
  return n < 10 ? 0 : Math.floor(n / 10)
}

// La page suit ce qui joue (télécommande infrarouge, +10, piste suivante) ;
// faute de présélection déclarée, un changement de compte ramène en première
// page. Un seul observateur pour les deux champs : ils arrivent dans la même
// trame. (Déplacé tel quel depuis HomeView, avec `immediate` en plus : ici
// `etat` arrive en prop déjà peuplée dès le montage — dans HomeView elle
// partait de `null` puis se peuplait plus tard, ce qui déclenchait le watch
// sans qu'il ait besoin d'un appel immédiat.)
watch([compte, presetActif], (_, [compteAvant]) => {
  if (presetActif.value !== null) {
    page.value = Math.min(pageDe(presetActif.value), dernierePage.value)
    return
  }
  if (compte.value !== compteAvant) page.value = 0
}, { immediate: true })

const grisees = computed(() => indisponible('Select', props.etat))
</script>

<template>
  <div class="space-y-3" data-grille-presets>
    <div v-if="compte !== null" class="flex items-center gap-2">
      <!-- Le libelle "Présélections" est deja le titre de la carte (HomeView) :
           ici, seul le compte. -->
      <p data-preset-count class="text-xs text-muted-foreground">{{ compte }}</p>
      <span class="flex-1" />
      <template v-if="paginationVisible">
        <Button data-preset-prev variant="outline" size="icon-sm" :disabled="page === 0" :aria-label="t('presets_prev_page')" @click="pagePrecedente">
          <ChevronLeftIcon class="size-4" />
        </Button>
        <span class="text-xs tabular-nums text-muted-foreground" data-preset-fenetre>{{ fenetre }}</span>
        <Button data-preset-next variant="outline" size="icon-sm" :disabled="page === dernierePage" :aria-label="t('presets_next_page')" @click="pageSuivante">
          <ChevronRightIcon class="size-4" />
        </Button>
      </template>
    </div>
    <!-- Une tuile = numero + nom. Deux colonnes : assez pour un nom de station,
         et la meme grille sur telephone et dans la demi-largeur du PC. -->
    <div class="grid grid-cols-2 gap-2">
      <Button
        v-for="n in presets"
        :key="n"
        :data-preset-button="n"
        :data-preset-active="etat?.preset === n ? 'true' : undefined"
        :aria-current="etat?.preset === n ? 'true' : undefined"
        :variant="etat?.preset === n ? 'default' : 'outline'"
        class="h-14 justify-start gap-3 px-3 md:h-12"
        :disabled="grisees"
        @click="emit('choisir', n)"
      >
        <span class="w-6 text-left text-base font-bold" :class="etat?.preset === n ? '' : 'text-muted-foreground'">{{ n }}</span>
        <span v-if="nomDe(n)" class="truncate font-medium" data-preset-name>{{ nomDe(n) }}</span>
        <span v-if="etat?.preset === n" class="ml-auto size-2 shrink-0 rounded-full bg-current" aria-hidden="true" />
      </Button>
    </div>
  </div>
</template>
