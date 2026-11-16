<script setup lang="ts">
import { Badge, Button, Slider } from '@ritornello/ui'
import { SpeakerLoudIcon, SpeakerOffIcon } from '@radix-icons/vue'
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'

// Volume is a continuous setting: a slider, not two keys. The keyboard
// (arrows = 5 %, Page and Shift+arrows = 10 %, Home/End) is handled here, not
// by reka, whose Page keys would jump by 50 with a step of 5; reka's
// `role=slider` still carries the accessibility semantics. The − / + keys
// remain those of the physical remote. The command sent is `SetVolume`
// (absolute): at once on a key press; during a drag, the first value at once,
// then at most one every THROTTLE_MS carrying the finger's latest value; on
// release, the final value at once if it was not the last one sent.
const STEP = 5
const THROTTLE_MS = 150
const LATCH_TIMEOUT_MS = 2000
const { t } = useCatalog()
const props = defineProps<{ volume: number | null; muted: boolean; disabled: boolean }>()
const emit = defineEmits<{ set: [percent: number]; mute: [] }>()

// Value under the finger during the drag; null outside a gesture. While it is
// set, the display follows it and nothing else.
const local = ref<number | null>(null)
// Released value, held on screen until the device confirms it. Same reason as
// in ProgressBar: a frame from before the adjustment must not make the handle
// step back for an instant. Sending while dragging makes it worse: the echoes
// of the intermediate sends come back after the release, and each would pull
// the thumb back to a value the finger has already left.
const target = ref<number | null>(null)
// Values sent and not yet seen in a frame, oldest first. Not reactive: nothing
// renders from it; it is read by the frame watcher and by `finish`.
let inFlight: number[] = []
// Last value sent in the current gesture, and when. Reset at every release:
// dedup and throttle are per gesture — a stale `lastSent` would swallow a tap
// on a value a remote has since left.
let lastSent: number | null = null
let lastSentAt = -Infinity
let trailing: ReturnType<typeof setTimeout> | null = null
let backstop: ReturnType<typeof setTimeout> | null = null

function sendNow(p: number): void {
  emit('set', p)
  lastSent = p
  lastSentAt = Date.now()
  inFlight.push(p)
}

function clearTrailing(): void {
  if (trailing !== null) clearTimeout(trailing)
  trailing = null
}

function release(): void {
  target.value = null
  inFlight = []
  if (backstop !== null) clearTimeout(backstop)
  backstop = null
}

// Frames. Strict equality, not ProgressBar's tolerance to the `step`: the
// volume is an exact integer the core returns as is, so the echo of a send
// lands exactly on it. A frame is recognised as an echo of ours when it is in
// `inFlight`, and the **first** occurrence is the one it answers: echoes come
// back in send order, so after sends 50, 45, 50 the first frame 50 answers
// the first send, not the last — releasing on it would let the echo of 45
// drop the thumb. That echo confirms every older send too (the core may
// coalesce frames and skip some), so they all leave the queue. A frame that
// is not in the queue comes from another source (infrared remote, an MPD
// client) and releases the latch at once, otherwise the page would stay
// frozen on its own value. The latch also lets go when the queue is empty:
// the final send's echo has arrived. During a drag the queue is consumed the
// same way, but the display stays on `local` regardless.
// Known limit, bounded and self-correcting: `inFlight` may also hold values
// that `latestWins` (in HomeView) dropped without ever POSTing. If the remote
// or MPD lands on one of them before the final echo, that frame is taken for
// an echo and the latch holds the released value until the final echo or the
// 2 s backstop. Likewise when the final echo does not change `props.volume`
// (60 -> 55 -> 60 coalesced): the watcher does not fire, only the backstop
// releases.
watch(
  () => props.volume,
  (v) => {
    if (v === null) return
    const i = inFlight.indexOf(v)
    if (i >= 0) inFlight.splice(0, i + 1)
    if (target.value !== null && (i < 0 || inFlight.length === 0)) release()
  },
)
const displayed = computed(() => local.value ?? target.value ?? props.volume)

// Drag (pointer only: the keyboard is captured below). Leading edge at once,
// then a trailing timer that sends the finger's latest value, not the one
// that armed it.
// reka's `update:modelValue` may emit `undefined` (case of a removed handle,
// outside our single-handle usage): the type allows for it, not our logic —
// we then fall back to 0 without crashing.
function onChange(v: number[] | undefined): void {
  local.value = Math.round(v?.[0] ?? 0)
  if (trailing !== null) return
  if (local.value === lastSent) return
  const elapsed = Date.now() - lastSentAt
  if (elapsed >= THROTTLE_MS) {
    sendNow(local.value)
    return
  }
  trailing = setTimeout(() => {
    trailing = null
    if (local.value !== null && local.value !== lastSent) sendNow(local.value)
  }, THROTTLE_MS - elapsed)
}

// End of a gesture: reka's `valueCommit`, a key press, or `pointerup` /
// `pointercancel` on the wrapper — reka emits no `valueCommit` when a drag
// ends on the value it started from. The wrapper's listener runs after reka's
// own, so after a `valueCommit` it finds `local === null` and does nothing.
function finish(v: number): void {
  clearTrailing()
  const p = Math.round(v)
  local.value = null
  if (p !== lastSent) sendNow(p)
  // Nothing unconfirmed: nothing to hold.
  target.value = inFlight.length > 0 ? p : null
  if (target.value !== null) {
    if (backstop !== null) clearTimeout(backstop)
    // A lost echo must not freeze the display forever.
    backstop = setTimeout(release, LATCH_TIMEOUT_MS)
  }
  lastSent = null
  lastSentAt = -Infinity
}
function onCommit(v: number[]): void {
  finish(v[0] ?? 0)
}
function onPointerEnd(): void {
  if (local.value !== null) finish(local.value)
}

onBeforeUnmount(() => {
  clearTrailing()
  if (backstop !== null) clearTimeout(backstop)
  backstop = null
})

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
  if (dest !== base) finish(dest)
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
    <div
      class="flex-1"
      @keydown.capture="fromKeyboard"
      @pointerup="onPointerEnd"
      @pointercancel="onPointerEnd"
    >
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
