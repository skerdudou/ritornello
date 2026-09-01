<script setup lang="ts">
import { Badge, Button, Slider } from '@ritornello/ui'
import { SpeakerLoudIcon, SpeakerOffIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'

// Volume is a continuous setting: a slider, not two keys. The keyboard
// (arrows = 1 %, Page = 10 %, Home/End) and reka's `role=slider` cover
// accessibility; the − / + keys remain those of the physical remote. The
// command sent is `SetVolume` (absolute), a single one on release — during
// the gesture, only the display moves.
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
function onCommit(v: number[]): void {
  const p = Math.round(v[0] ?? 0)
  local.value = null
  target.value = p
  emit('set', p)
}
</script>

<template>
  <div class="flex items-center gap-3" data-volume-ligne>
    <!-- The icon **is** the toggle: that is where one looks for the sound. -->
    <Button
      variant="ghost"
      size="icon"
      data-remote-command="Mute"
      :data-actif="muted ? 'true' : undefined"
      :aria-pressed="String(muted)"
      :aria-label="t('remote_mute')"
      :disabled="disabled"
      @click="emit('mute')"
    >
      <SpeakerOffIcon v-if="muted" class="size-5" />
      <SpeakerLoudIcon v-else class="size-5" />
    </Button>
    <Slider
      class="flex-1"
      data-volume-curseur
      :model-value="[displayed ?? 0]"
      :min="0"
      :max="100"
      :step="1"
      :disabled="disabled || displayed === null"
      :aria-label="t('volume')"
      @update:model-value="onChange"
      @value-commit="onCommit"
    />
    <span
      class="w-12 text-right text-sm tabular-nums text-foreground"
      :class="{ 'line-through opacity-60': muted }"
      data-volume
      >{{ displayed === null ? '' : displayed + ' %' }}</span
    >
    <Badge v-if="muted" variant="secondary" data-muted>{{ t('muted') }}</Badge>
  </div>
</template>
