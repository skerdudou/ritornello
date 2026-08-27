<script setup lang="ts">
import { Badge, Button, Slider } from '@ritornello/ui'
import { SpeakerLoudIcon, SpeakerOffIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'

// Le volume est un reglage continu : un curseur, pas deux touches. Le clavier
// (fleches = 1 %, Page = 10 %, Debut/Fin) et le `role=slider` de reka couvrent
// l'accessibilite ; les touches − / + restent celles de la telecommande
// physique. La commande envoyee est `SetVolume` (absolue), une seule au
// relachement — pendant le geste, seul l'affichage bouge.
const { t } = useCatalog()
const props = defineProps<{ volume: number | null; muted: boolean; desactive: boolean }>()
const emit = defineEmits<{ regler: [pourcent: number]; muet: [] }>()

// Valeur sous le doigt pendant le glisser ; null hors geste.
const locale = ref<number | null>(null)
// Valeur validee, en attente de la trame qui la confirme. Meme raison que
// dans BarreProgression : la trame d'avant le reglage ne doit pas faire
// reculer la poignee un instant.
const visee = ref<number | null>(null)
watch(
  () => props.volume,
  (v, avant) => {
    if (visee.value === null) return
    // Egalite stricte, pas la tolerance au `pas` de BarreProgression : le
    // volume est un entier exact que le coeur renvoie tel quel, sans lissage
    // ni arrondi cote appareil — la trame qui confirme tombe forcement pile.
    // Mais une source externe (telecommande infrarouge) fait aussi bouger le
    // volume sans jamais tomber sur `visee` : tout changement du volume reel
    // (par rapport a la trame precedente) prouve que l'appareil a parle, et
    // relache la visee. Une trame en vol qui repete encore l'ancienne valeur
    // ne change pas `avant` -> `v`, donc ne relache rien a tort.
    if (v === visee.value || (avant !== undefined && v !== avant)) visee.value = null
  },
)
const affiche = computed(() => locale.value ?? visee.value ?? props.volume)

// `update:modelValue` de reka peut emettre `undefined` (cas d'une poignee
// retiree, hors de notre usage a une seule poignee) : le type le prevoit, pas
// notre logique — on retombe alors sur 0 sans planter.
function surChangement(v: number[] | undefined): void {
  locale.value = v?.[0] ?? 0
}
function surValidation(v: number[]): void {
  const p = Math.round(v[0] ?? 0)
  locale.value = null
  visee.value = p
  emit('regler', p)
}
</script>

<template>
  <div class="flex items-center gap-3" data-volume-ligne>
    <!-- L'icône **est** la bascule : c'est là qu'on cherche le son. -->
    <Button
      variant="ghost"
      size="icon"
      data-remote-command="Mute"
      :data-actif="muted ? 'true' : undefined"
      :aria-pressed="String(muted)"
      :aria-label="t('remote_mute')"
      :disabled="desactive"
      @click="emit('muet')"
    >
      <SpeakerOffIcon v-if="muted" class="size-5" />
      <SpeakerLoudIcon v-else class="size-5" />
    </Button>
    <Slider
      class="flex-1"
      data-volume-curseur
      :model-value="[affiche ?? 0]"
      :min="0"
      :max="100"
      :step="1"
      :disabled="desactive || affiche === null"
      :aria-label="t('volume')"
      @update:model-value="surChangement"
      @value-commit="surValidation"
    />
    <span
      class="w-12 text-right text-sm tabular-nums text-foreground"
      :class="{ 'line-through opacity-60': muted }"
      data-volume
      >{{ affiche === null ? '' : affiche + ' %' }}</span
    >
    <Badge v-if="muted" variant="secondary" data-muted>{{ t('muted') }}</Badge>
  </div>
</template>
