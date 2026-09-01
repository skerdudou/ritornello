<script setup lang="ts">
import { Slider } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { formatDuration, formatPosition } from '../composables/usePlayer'

// Three states, and the payload tells them all apart:
//  - unknown position: nothing (a radio without a metadata plugin);
//  - position known but not `seekable`: a bar that informs, without handle
//    nor role (Radio France announces the duration of a live stream you
//    cannot rewind);
//  - `seekable`: a real slider, by finger as by keyboard.
const { t } = useCatalog()
const props = defineProps<{
  position: number | null
  duration: number | null
  /** The content accepts a seek (`seekable` of the payload). */
  seekable: boolean
  /** Keyboard step, in seconds: the same as the physical keys'. */
  step: number
}>()
const emit = defineEmits<{ seek: [seconds: number] }>()

// Value under the finger during the drag; null outside a gesture.
const local = ref<number | null>(null)
// Committed value, awaiting the frame that confirms it. Without it, the next
// frame — the one from before the jump, already on its way — brought the
// handle back for an instant.
const target = ref<number | null>(null)
watch(
  () => props.position,
  (p) => {
    if (target.value === null) return
    // Frame without position (end of track, Stop, standby, source change):
    // nothing will ever confirm the jump, the target value must be released
    // right away rather than freezing the bar on a dead target.
    if (p == null) {
      target.value = null
      return
    }
    if (Math.abs(p - target.value) <= props.step) target.value = null
  },
)
const displayed = computed(() => local.value ?? target.value ?? props.position)

const elapsedText = computed(() => formatPosition(displayed.value))
// formatDuration, not formatPosition: the latter accepts zero, whereas a total
// duration of "0:00" is not one (see its doc in usePlayer.ts).
const durationText = computed(() => formatDuration(props.duration))
// An endless bar teaches nothing: without a known duration, only the elapsed time is shown.
const barVisible = computed(() => props.duration != null && props.duration > 0)
const percent = computed(() => {
  if (!barVisible.value || displayed.value == null) return 0
  return Math.min(100, Math.max(0, (displayed.value / (props.duration as number)) * 100))
})

// reka's `update:modelValue` may emit `undefined` (case of a removed handle,
// outside our single-handle usage): the type allows for it, not our logic —
// we then fall back to 0 without crashing.
function onChange(v: number[] | undefined): void {
  local.value = v?.[0] ?? 0
}

function onCommit(v: number[]): void {
  const s = Math.round(v[0] ?? 0)
  local.value = null
  target.value = s
  emit('seek', s)
}

// The keyboard keeps the step of the physical keys, not the slider's one
// second: captured on the wrapper, before reka's handler on the handle.
function fromKeyboard(e: KeyboardEvent): void {
  if (!props.seekable || props.duration == null) return
  // From the confirmed position (`position`), not from `displayed`: the target
  // value may still be refused by the device, and starting from it would make
  // the following steps drift from an unconfirmed hypothesis rather than from
  // the known truth.
  const from = props.position ?? 0
  const dest = {
    ArrowRight: from + props.step,
    ArrowUp: from + props.step,
    ArrowLeft: from - props.step,
    ArrowDown: from - props.step,
    Home: 0,
    End: props.duration,
  }[e.key]
  if (dest === undefined) return
  e.preventDefault()
  e.stopPropagation()
  onCommit([Math.min(props.duration, Math.max(0, dest))])
}
</script>

<template>
  <!--
    Owner's request: far too much air before and after the bar, the durations
    could be almost glued to the track. `-mt-3` brings the bar closer to the
    text block above: the `gap-6` (24 px) of the kit's `Card` separating the
    two `CardContent` of `PlayerCard` cannot be changed here without touching
    a shared component, so it is compensated on the `ProgressBar` side —
    measured on screen (Playwright, 390 px) after a first attempt at `-mt-2`:
    it only gave back 8 of the 24 px, insufficient. The original `space-y-1`
    is removed in favour of a `mt-0.5` carried by the durations line alone,
    closer to what it must hug.

    **`-mt-3` and not `-mt-4`**: the tightening had gone too far. At 16 px of
    compensation only 8 px remained above the track, and the owner reported it
    as glued "to the pixel". Twelve px compensated leave twelve, which is the
    very small gap requested — the durations line, for its part, keeps its
    2 px and stays against the track, that is indeed what he wanted.
    `flex flex-col` on the root: an ordinary block merges its margin with its
    first child's (CSS margin collapsing) — here the slider's wrapper, whose
    `Slider` `-my-[19px]` then went up alone (CSS keeps only the most negative
    of the two, -19px, instead of adding them to -mt-4). Measured on screen:
    24 px remained instead of 8 on the file case. A flex container does not
    merge its margins with its children, which restores the expected addition;
    the static bar, without a margin of its own, is not affected.
  -->
  <div v-if="elapsedText" class="-mt-3 flex flex-col" data-progression>
    <div v-if="barVisible && seekable" @keydown.capture="fromKeyboard">
      <!--
        `-my-[19px]`: the slider keeps its 44 px touch area (the kit's
        `py-[19px]`, untouched) without making the layout pay for it.
        Measured on screen: `-my-3` (12 px) only compensated 12 of the 19 px
        of padding, still leaving 31 px between the text block and the track
        on the file case. By reclaiming the whole padding, the slider's visual
        box becomes the 6 px track again, like the static bar — the touch area
        then overflows entirely onto its neighbours: the durations line below
        (text, never a target, it stays inert in the full sense) and the
        badges line above, which IS NOT inert when it carries platform links
        (`data-lien`, 44 px anchors, cf. PlayerCard.vue) — the 19 px overflow
        would cover their bottom and steal their tap. It is PlayerCard.vue that
        brings them in front in the paint order (`relative z-10` on
        `[data-links]`) to give the tap back to the links; slider side, nothing
        else to do here. The handle stays clickable at the edge: the negative
        margin moves the box, not the touch area it contains (the padding
        itself does not move).
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
    <!-- `py-0`, not `py-[19px]`: this bar is not a target (not `seekable`),
         so it has no touch area to reserve — and `py-0` rather than `py-1`
         so that radio (static bar) and file (slider) share exactly the same
         geometry, the 6 px track sticking directly to its neighbours in both
         cases. -->
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
