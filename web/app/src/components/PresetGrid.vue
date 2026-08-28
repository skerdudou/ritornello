<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ChevronLeftIcon, ChevronRightIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'
import { unavailable } from '../views/remoteCommands'

const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null; nameOf: (n: number) => string | null }>()
const emit = defineEmits<{ choose: [n: number] }>()

const page = ref(0)

// Compte déclaré par la source (null = source muette sur le sujet : grille
// 1-10, pour ne jamais désarmer la télécommande).
const count = computed(() => props.state?.preset_count ?? null)

// Numéros de la page courante, seulement ceux qui existent. Page k :
// 10k+1 à 10k+10 — donc 1-10, 11-20, 21-30. **Mêmes bornes que le `+10` du
// cœur**, et ce n'est step une coïncidence à entretenir mais une contrainte :
// la page web et la touche physique doivent désigner les mêmes groupes, sans
// quoi « page 2 » ne veut step dire la même chose selon qu'on regarde l'écran
// ou la télécommande. Côté cœur, la touche 0 vaut dix (voir
// `Command::Select`), ce qui est exactement ce qui fait tenir dix numéros dans
// une dizaine.
//
// Ne step « simplifier » en fenêtres de 6 ou 12 pour des raisons de mise en
// page : le pavé numérique n'a que dix chiffres.
const presets = computed(() => {
  const c = count.value
  if (c === null) return Array.from({ length: 10 }, (_, i) => i + 1)
  const debut = page.value * 10 + 1
  const fin = Math.min(page.value * 10 + 10, c)
  return debut > fin ? [] : Array.from({ length: fin - debut + 1 }, (_, i) => debut + i)
})

const paginationVisible = computed(() => (count.value ?? 0) > 10)

// Dernière page non vide. Une page k couvre 10k+1..10k+10, donc la dernière
// qui contient quelque chose est `floor((count - 1) / 10)` — même borne que
// le rebouclage du `+10` côté cœur, qui s'écrit là-bas `((count - 1) / 10) * 10`.
const lastPage = computed(() => {
  const c = count.value ?? 0
  return c > 0 ? Math.floor((c - 1) / 10) : 0
})

const window = computed(() => {
  const p = presets.value
  return p.length ? `${p[0]}–${p[p.length - 1]}` : ''
})

function previousPage() {
  if (page.value > 0) page.value -= 1
}
function nextPage() {
  if (page.value < lastPage.value) page.value += 1
}

const activePreset = computed(() => props.state?.preset ?? null)

// La page qui contient la présélection `n` (1-based). `n - 1` parce qu'une
// page couvre 10k+1..10k+10 : 10 appartient à la page 0, 11 à la page 1.
function pageOf(n: number) {
  return Math.floor((n - 1) / 10)
}

// La page suit ce qui joue (télécommande infrarouge, +10, piste suivante) ;
// faute de présélection déclarée, un changement de count ramène en première
// page. Un seul observateur pour les deux fields : ils arrivent dans la même
// trame. (Déplacé tel quel depuis HomeView, avec `immediate` en plus : ici
// `state` arrive en prop déjà peuplée dès le montage — dans HomeView elle
// partait de `null` puis se peuplait plus tard, ce qui déclenchait le watch
// sans qu'il ait besoin d'un appel immédiat.)
watch([count, activePreset], (_, [compteAvant]) => {
  if (activePreset.value !== null) {
    page.value = Math.min(pageOf(activePreset.value), lastPage.value)
    return
  }
  if (count.value !== compteAvant) page.value = 0
}, { immediate: true })

const dimmed = computed(() => unavailable('Select', props.state))
</script>

<template>
  <div class="space-y-3" data-grille-presets>
    <div v-if="count !== null" class="flex items-center gap-2">
      <!-- Le label "Présélections" est deja le titre de la carte (HomeView) :
           ici, seul le count. -->
      <p data-preset-count class="text-xs text-muted-foreground">{{ count }}</p>
      <span class="flex-1" />
      <template v-if="paginationVisible">
        <Button data-preset-prev variant="outline" size="icon-sm" :disabled="page === 0" :aria-label="t('presets_prev_page')" @click="previousPage">
          <ChevronLeftIcon class="size-4" />
        </Button>
        <span class="text-xs tabular-nums text-muted-foreground" data-preset-window>{{ window }}</span>
        <Button data-preset-next variant="outline" size="icon-sm" :disabled="page === lastPage" :aria-label="t('presets_next_page')" @click="nextPage">
          <ChevronRightIcon class="size-4" />
        </Button>
      </template>
    </div>
    <!-- Une tuile = numero + nom. Deux colonnes : assez pour un nom de station,
         et la meme grille sur phone et dans la demi-largeur du PC. -->
    <div class="grid grid-cols-2 gap-2">
      <Button
        v-for="n in presets"
        :key="n"
        :data-preset-button="n"
        :data-preset-active="state?.preset === n ? 'true' : undefined"
        :aria-current="state?.preset === n ? 'true' : undefined"
        :variant="state?.preset === n ? 'default' : 'outline'"
        class="h-14 justify-start gap-3 px-3 md:h-12"
        :disabled="dimmed"
        @click="emit('choose', n)"
      >
        <span class="w-6 text-left text-base font-bold" :class="state?.preset === n ? '' : 'text-muted-foreground'">{{ n }}</span>
        <span v-if="nameOf(n)" class="truncate font-medium" data-preset-name>{{ nameOf(n) }}</span>
        <span v-if="state?.preset === n" class="ml-auto size-2 shrink-0 rounded-full bg-current" aria-hidden="true" />
      </Button>
    </div>
  </div>
</template>
