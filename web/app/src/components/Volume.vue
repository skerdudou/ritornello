<script setup lang="ts">
import { Badge, Button, Slider } from '@ritornello/ui'
import { SpeakerLoudIcon, SpeakerOffIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'

// Volume is a continuous setting: a slider, not two keys. The keyboard
// (arrows = 5 %, Page and Shift+arrows = 10 %, Home/End) is handled here, not
// by reka, whose Page keys would jump by 50 with a step of 5; reka's
// `role=slider` still carries the accessibility semantics. The − / + keys
// remain those of the physical remote. The command sent is `SetVolume`
// (absolute): at once on a key press, a single one on release of a drag —
// during the gesture, only the display moves.
const STEP = 5
const { t } = useCatalog()
const props = defineProps<{ volume: number | null; muted: boolean; disabled: boolean }>()
const emit = defineEmits<{ set: [percent: number]; mute: [] }>()

// Value under the finger during the drag; null outside a gesture.
const local = ref<number | null>(null)
// Committed value, awaiting the frame that confirms it. Same reason as in
// ProgressBar: the frame from before the adjustment must not make the handle
// step back for an instant.
const target = ref<number | null>(null)
watch(
  () => props.volume,
  (v, previous) => {
    if (target.value === null) return
    // Strict equality, not ProgressBar's tolerance to the `step`: the volume
    // is an exact integer the core returns as is, without smoothing nor
    // rounding device side — the confirming frame necessarily lands exactly.
    // But an external source (infrared remote) also moves the volume without
    // ever landing on `target`: any change of the real volume (relative to
    // the previous frame) proves the device has spoken, and releases the
    // target. An in-flight frame still repeating the old value does not
    // change `previous` -> `v`, so releases nothing wrongly.
    if (v === target.value || (previous !== undefined && v !== previous)) target.value = null
  },
)
const displayed = computed(() => local.value ?? target.value ?? props.volume)

// reka's `update:modelValue` may emit `undefined` (case of a removed handle,
// outside our single-handle usage): the type allows for it, not our logic —
// we then fall back to 0 without crashing.
function onChange(v: number[] | undefined): void {
  local.value = v?.[0] ?? 0
}
function commit(p: number): void {
  local.value = null
  target.value = p
  emit('set', p)
}
function onCommit(v: number[]): void {
  commit(Math.round(v[0] ?? 0))
}

const UP_KEYS = ['ArrowRight', 'ArrowUp', 'PageUp']
const DOWN_KEYS = ['ArrowLeft', 'ArrowDown', 'PageDown']

// The keyboard is ours, not reka's (its Page keys jump by 50 with a step of 5,
// and it commits on its own schedule): we intercept in the capture phase, so
// reka never sees a key we handle. Moves land on the grid of 5 from an
// off-grid volume:
//   arrow up        = floor(base / 5) * 5 + 5    (33 -> 35)
//   arrow down      = ceil(base / 5) * 5 - 5     (33 -> 30)
//   Page, Shift+arrow = floor(base / 5) * 5 +/- 10 (33 -> 40 / 20)
// The base is the displayed value, not the confirmed `props.volume` as in
// ProgressBar: two quick presses must add up before any frame comes back, and
// the core always applies a clamped `SetVolume`, so the latched value is
// reliable.
function fromKeyboard(e: KeyboardEvent): void {
  if (props.disabled || displayed.value === null) return
  const base = displayed.value
  const big = e.key === 'PageUp' || e.key === 'PageDown' || e.shiftKey
  const floor = Math.floor(base / STEP) * STEP
  let dest: number
  if (UP_KEYS.includes(e.key)) dest = floor + (big ? 2 : 1) * STEP
  else if (DOWN_KEYS.includes(e.key)) dest = big ? floor - 2 * STEP : Math.ceil(base / STEP) * STEP - STEP
  else if (e.key === 'Home') dest = 0
  else if (e.key === 'End') dest = 100
  else return
  e.preventDefault()
  e.stopPropagation()
  dest = Math.min(100, Math.max(0, dest))
  if (dest !== base) commit(dest)
}
</script>

<template>
  <div class="flex items-center gap-3" data-volume-row>
    <!-- The icon **is** the toggle: that is where one looks for the sound. -->
    <Button
      variant="ghost"
      size="icon"
      data-remote-command="Mute"
      :data-on="muted ? 'true' : undefined"
      :aria-pressed="String(muted)"
      :aria-label="t('remote_mute')"
      :disabled="disabled"
      @click="emit('mute')"
    >
      <SpeakerOffIcon v-if="muted" class="size-5" />
      <SpeakerLoudIcon v-else class="size-5" />
    </Button>
    <div class="flex-1" @keydown.capture="fromKeyboard">
      <Slider
        data-volume-slider
        :model-value="[displayed ?? 0]"
        :min="0"
        :max="100"
        :step="STEP"
        :disabled="disabled || displayed === null"
        :aria-label="t('volume')"
        @update:model-value="onChange"
        @value-commit="onCommit"
      />
    </div>
    <span
      class="w-12 text-right text-sm tabular-nums text-foreground"
      :class="{ 'line-through opacity-60': muted }"
      data-volume
      >{{ displayed === null ? '' : displayed + ' %' }}</span
    >
    <Badge v-if="muted" variant="secondary" data-muted>{{ t('muted') }}</Badge>
  </div>
</template>
