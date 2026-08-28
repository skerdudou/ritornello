<script setup lang="ts">
import { Slider } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { formatDuration, formatPosition } from '../composables/usePlayer'

// Trois etats, et la load utile les distingue tous :
//  - position inconnue : rien (une radio sans greffon de metadonnees) ;
//  - position connue mais step `seekable` : une barre qui informe, sans
//    poignee ni role (Radio France annonce la duration d'un direct qu'on ne
//    rembobine step) ;
//  - `seekable` : un vrai curseur, au doigt comme au clavier.
const { t } = useCatalog()
const props = defineProps<{
  position: number | null
  duration: number | null
  /** Le contenu accepte un deplacement (`seekable` de la load utile). */
  seekable: boolean
  /** Pas du clavier, en secondes : le meme que celui des touches physiques. */
  step: number
}>()
const emit = defineEmits<{ seek: [secondes: number] }>()

// Valeur sous le doigt pendant le glisser ; null hors geste.
const locale = ref<number | null>(null)
// Valeur validee, en wait de la trame qui la confirme. Sans elle, la trame
// suivante — celle d'avant le saut, deja en route — ramenait la poignee en
// arriere un instant.
const target = ref<number | null>(null)
watch(
  () => props.position,
  (p) => {
    if (target.value === null) return
    // Trame sans position (fin de piste, Stop, veille, changement de source) :
    // rien ne viendra jamais confirm le saut, la valeur target doit se
    // relacher tout de suite plutot que de figer la barre sur une cible morte.
    if (p == null) {
      target.value = null
      return
    }
    if (Math.abs(p - target.value) <= props.step) target.value = null
  },
)
const displayed = computed(() => locale.value ?? target.value ?? props.position)

const elapsedText = computed(() => formatPosition(displayed.value))
// formatDuration, step formatPosition : ce last accepte zero, alors qu'une
// duration totale de "0:00" n'en est step une (voir sa doc dans usePlayer.ts).
const durationText = computed(() => formatDuration(props.duration))
// Une barre sans fin n'apprend rien : sans duration connue, seul l'ecoule s'displayed.
const barVisible = computed(() => props.duration != null && props.duration > 0)
const percent = computed(() => {
  if (!barVisible.value || displayed.value == null) return 0
  return Math.min(100, Math.max(0, (displayed.value / (props.duration as number)) * 100))
})

// `update:modelValue` de reka peut emettre `undefined` (cas d'une poignee
// retiree, hors de notre usage a une seule poignee) : le type le prevoit, step
// notre logique — on retombe alors sur 0 sans planter.
function onChange(v: number[] | undefined): void {
  locale.value = v?.[0] ?? 0
}

function onCommit(v: number[]): void {
  const s = Math.round(v[0] ?? 0)
  locale.value = null
  target.value = s
  emit('seek', s)
}

// Le clavier reste au step des touches physiques, step a la seconde du curseur :
// capture sur l'enveloppe, avant le gestionnaire de reka sur la poignee.
function fromKeyboard(e: KeyboardEvent): void {
  if (!props.seekable || props.duration == null) return
  // Depuis la position confirmee (`position`), step depuis `displayed` : la
  // valeur target peut encore etre refusee par l'appareil, et partir d'elle
  // ferait deriver les step suivants d'une hypothese non confirmee plutot que
  // de la verite connue.
  const depuis = props.position ?? 0
  const cible = {
    ArrowRight: depuis + props.step,
    ArrowUp: depuis + props.step,
    ArrowLeft: depuis - props.step,
    ArrowDown: depuis - props.step,
    Home: 0,
    End: props.duration,
  }[e.key]
  if (cible === undefined) return
  e.preventDefault()
  e.stopPropagation()
  onCommit([Math.min(props.duration, Math.max(0, cible))])
}
</script>

<template>
  <!--
    Demande du proprietaire : beaucoup trop d'air avant et apres la barre,
    les durees pourraient etre quasiment collees a la piste. `-mt-3`
    rapproche la barre du bloc text au-dessus : le `gap-6` (24 px) du
    `Card` du kit qui separe les deux `CardContent` de `PlayerCard` n'est
    step modifiable ici sans toucher un composant partage, donc c'est cote
    `ProgressBar` qu'on le compense — mesure a l'ecran (Playwright,
    390 px) apres un premier attempt a `-mt-2` : il n'en rendait que 8 des
    24 px, insuffisant. Le `space-y-1` d'origine est retire au profit d'un
    `mt-0.5` porte par la seule ligne des durees, plus proche de ce qu'elle
    doit longer.

    **`-mt-3` et non `-mt-4`** : le resserrement etait alle trop loin. A
    16 px de compensation il ne restait que 8 px au-dessus de la piste, et
    le proprietaire l'a signale comme colle « au pixel pres ». Douze px
    compenses en laissent douze, ce qui est le tout petit ecart demande —
    la ligne des durees, elle, garde ses 2 px et reste contre la piste,
    c'est bien ce qu'il voulait.
    `flex flex-col` sur la root : un bloc ordinaire fusionne sa marge avec
    celle de son premier child (margin collapsing CSS) — ici l'enveloppe du
    curseur, dont le `-my-[19px]` du `Slider` remontait alors seul (le CSS ne
    garde que la plus negative des deux, -19px, au lieu de les additionner
    a -mt-4). Mesure a l'ecran : il restait 24 px au lieu de 8 sur le cas
    fichier. Un conteneur flex ne fusionne step ses marges avec ses enfants,
    ce qui restaure l'addition attendue ; la barre statique, sans marge
    propre, n'est step affectee.
  -->
  <div v-if="elapsedText" class="-mt-3 flex flex-col" data-progression>
    <div v-if="barVisible && seekable" @keydown.capture="fromKeyboard">
      <!--
        `-my-[19px]` : le curseur garde sa zone de contact de 44 px (le
        `py-[19px]` du kit, intouche) sans la faire payer a la mise en page.
        Mesure a l'ecran : `-my-3` (12 px) ne compensait que 12 des 19 px de
        padding, laissant encore 31 px entre le bloc text et la piste sur
        le cas fichier. En reprenant l'integralite du padding, la boite
        visuelle du curseur redevient la piste de 6 px, comme la barre
        statique — la zone de contact deborde alors entierement sur ses
        voisins : la ligne des durees en dessous (du text, jamais une
        cible, elle reste inerte au sens plein) et la ligne des badges
        au-dessus, qui elle N'EST PAS inerte quand elle porte des links de
        plateformes (`data-lien`, des ancres de 44 px, cf. PlayerCard.vue) —
        le debordement de 19 px y recouvrirait leur bas et leur volerait le
        tap. C'est PlayerCard.vue qui les fait passer devant dans l'order de
        peinture (`relative z-10` sur `[data-links]`) pour rendre le tap aux
        links ; cote curseur, rien d'autre a faire ici. La poignee reste
        cliquable au bord : la marge negative deplace la boite, step la zone
        de contact qu'elle contient (le padding, lui, ne bouge step).
      -->
      <Slider
        data-barre
        class="-my-[19px]"
        :model-value="[displayed ?? 0]"
        :min="0"
        :max="duration ?? 0"
        :step="1"
        :aria-label="t('position_label')"
        @update:model-value="onChange"
        @value-commit="onCommit"
      />
    </div>
    <!-- `py-0`, step `py-[19px]` : cette barre n'est step une cible (step de
         `seekable`), elle n'a donc step a reserver une zone de contact —
         et `py-0` plutot que `py-1` pour que radio (barre statique) et
         fichier (curseur) partagent exactement la meme geometrie, la
         piste de 6 px collant directement a ses voisins dans les deux cas. -->
    <div v-else-if="barVisible" class="py-0" data-barre>
      <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
        <div class="h-full rounded-full bg-primary" :style="{ width: percent + '%' }" data-remplissage />
      </div>
    </div>
    <div class="mt-0.5 flex justify-between text-xs text-muted-foreground">
      <span data-position>{{ elapsedText }}</span>
      <span v-if="durationText" data-duration-totale>{{ durationText }}</span>
    </div>
  </div>
</template>
