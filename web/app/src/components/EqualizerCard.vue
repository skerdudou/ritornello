<script setup lang="ts">
/**
 * The ten-band equalizer, on the configuration page.
 *
 * Every valid change writes the **whole** settings (`PUT /api/equalizer` is a
 * full replacement: a body without `user_presets` would wipe them). Changes
 * made in quick succession — a slider dragged, a preset browsed — are
 * coalesced: one pending timer, reset by each change, and when it fires a
 * single `PUT` of the latest state. Writes are serial: a change that arrives
 * while a `PUT` is in flight is sent after it, never alongside, so the core
 * can never apply an older state last. A refusal shows the core's message and
 * reloads what the core holds, rather than leaving a state on screen that
 * nothing stored. The reload is the last word: the write lock is held until
 * it lands, and only then are the timer and the pending write dropped — a
 * change made meanwhile is lost with the refused state rather than writing
 * that state again on top of the reload (a timer may well fire before a slow
 * `GET` answers).
 *
 * "Custom" is not stored: it is shown when no preset is selected **or** when
 * the gains no longer match the selected preset's. `preset` itself is left as
 * it is when a slider moves, so moving it back names the preset again.
 *
 * The preset list renders its label itself (`<SelectValue>{{ … }}</SelectValue>`):
 * reka-ui reads an option's text once, at mount, and "Custom" is not even an
 * option (see the memory of `ConfigView`'s lists, same pattern).
 */
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, Input, Label,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Slider, Switch, toast,
} from '@ritornello/ui'
import { computed, onBeforeUnmount, onMounted, ref, toRaw } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { EqualizerBuiltin, EqualizerPayload, EqualizerSettings } from '../types'

const { t } = useCatalog()

/** Bounds mirrored from the core (`equalizer.rs`), so a slider cannot ask for a refusal. */
const GAIN_MIN = -12
const GAIN_MAX = 12
/** How long a change waits for the next one before it is written. */
const COALESCE_MS = 300
/** The value the list holds when the state is "Custom": no option carries it. */
const CUSTOM = '__custom__'

const state = ref<EqualizerSettings | null>(null)
const bandsHz = ref<number[]>([])
const builtins = ref<EqualizerBuiltin[]>([])

async function load() {
  try {
    const p = await api.get<EqualizerPayload>('/api/equalizer')
    state.value = p.settings
    bandsHz.value = p.bands_hz
    builtins.value = p.builtins
  } catch {
    // The card stays empty: the rest of the page must not depend on it.
  }
}

onMounted(load)

// --- Writing -----------------------------------------------------------

let timer: ReturnType<typeof setTimeout> | null = null
let inFlight = false
/** A write was asked for while another was in flight. */
let pending = false

function cancelTimer() {
  if (timer !== null) {
    clearTimeout(timer)
    timer = null
  }
}

/** A change was made: write it once the changes stop for `COALESCE_MS`. */
function schedule() {
  cancelTimer()
  timer = setTimeout(() => {
    timer = null
    void flush()
  }, COALESCE_MS)
}

/** Writes the latest state now (or right after the write in flight). */
async function flush() {
  cancelTimer()
  if (!state.value) return
  if (inFlight) {
    pending = true
    return
  }
  inFlight = true
  const err = await api.put('/api/equalizer', structuredClone(toRaw(state.value)))
  if (err) {
    toast.error(err)
    // Still "in flight" while reloading: a flush meanwhile only marks
    // `pending`, dropped below once the core's state is back on screen.
    await load()
    cancelTimer()
    pending = false
    inFlight = false
    return
  }
  inFlight = false
  if (pending) {
    pending = false
    await flush()
  }
}

// Leaving the page within the coalescing window must not lose the change.
onBeforeUnmount(() => {
  if (timer !== null) void flush()
})

// --- Presets -----------------------------------------------------------

const userKey = (name: string) => `user:${name}`

function presetGains(key: string | null): number[] | null {
  if (key === null) return null
  if (key.startsWith('builtin:')) {
    const id = key.slice('builtin:'.length)
    return builtins.value.find((b) => b.id === id)?.gains ?? null
  }
  if (key.startsWith('user:')) {
    const name = key.slice('user:'.length)
    return state.value?.user_presets.find((p) => p.name === name)?.gains ?? null
  }
  return null
}

const sameGains = (a: number[], b: number[]) =>
  a.length === b.length && a.every((g, i) => g === b[i])

/** The selected preset, when the gains are still exactly its own. */
const currentPreset = computed<string | null>(() => {
  const s = state.value
  if (!s) return null
  const gains = presetGains(s.preset)
  return gains && sameGains(gains, s.gains) ? s.preset : null
})

const presetLabel = computed(() => {
  const key = currentPreset.value
  if (key === null) return t.value('eq_custom')
  if (key.startsWith('user:')) return key.slice('user:'.length)
  const id = key.slice('builtin:'.length)
  return builtins.value.find((b) => b.id === id)?.name ?? key
})

function pickPreset(key: unknown) {
  const s = state.value
  if (!s || typeof key !== 'string' || key === CUSTOM) return
  const gains = presetGains(key)
  if (!gains) return
  s.gains = [...gains]
  s.preset = key
  schedule()
}

function resetFlat() {
  const s = state.value
  if (!s) return
  s.gains = s.gains.map(() => 0)
  s.preset = 'builtin:flat'
  schedule()
}

// --- Switches and sliders ----------------------------------------------

function setEnabled(v: boolean) {
  if (!state.value) return
  state.value.enabled = v
  schedule()
}

function setAutoPreamp(v: boolean) {
  if (!state.value) return
  state.value.auto_preamp = v
  schedule()
}

function setGain(band: number, value: number[] | undefined) {
  const s = state.value
  const g = value?.[0]
  if (!s || g === undefined || !Number.isInteger(g) || g < GAIN_MIN || g > GAIN_MAX) return
  s.gains[band] = g
  schedule()
}

const bandLabel = (hz: number) => (hz >= 1000 ? `${hz / 1000}k` : String(hz))
const signed = (g: number) => (g > 0 ? `+${g}` : String(g))

// --- Save under a name / delete ----------------------------------------

const naming = ref(false)
const name = ref('')
/** The user preset a save would replace, once the user was asked. */
const overwriting = ref<string | null>(null)
const trimmedName = computed(() => name.value.trim())

function openSave() {
  name.value = ''
  overwriting.value = null
  naming.value = true
}

function existingNamed(n: string) {
  const lower = n.toLowerCase()
  return state.value?.user_presets.find((p) => p.name.toLowerCase() === lower) ?? null
}

function submitName() {
  const n = trimmedName.value
  if (!n) return
  const clash = existingNamed(n)
  if (clash) {
    overwriting.value = clash.name
    return
  }
  save(n)
}

/** Stores the current gains under `n`, replacing (in place) a preset of the same name. */
function save(n: string) {
  const s = state.value
  if (!s) return
  const entry = { name: n, gains: [...s.gains] }
  const lower = n.toLowerCase()
  const at = s.user_presets.findIndex((p) => p.name.toLowerCase() === lower)
  if (at >= 0) s.user_presets.splice(at, 1, entry)
  else s.user_presets.push(entry)
  s.preset = userKey(n)
  naming.value = false
  overwriting.value = null
  void flush()
}

const deletable = computed(() => currentPreset.value?.startsWith('user:') ?? false)
const deleting = ref(false)
/**
 * The user preset the confirmation was opened on, frozen: the list's label
 * turns to "Custom" as soon as the deletion lands, and the closing dialog
 * must not read "Delete the preset “Custom”?".
 */
const deleteName = ref('')

function openDelete() {
  const key = currentPreset.value
  if (!key?.startsWith('user:')) return
  deleteName.value = key.slice('user:'.length)
  deleting.value = true
}

function confirmDelete() {
  const s = state.value
  const n = deleteName.value
  deleting.value = false
  if (!s || !s.user_presets.some((p) => p.name === n)) return
  s.user_presets = s.user_presets.filter((p) => p.name !== n)
  s.preset = null
  void flush()
}
</script>

<template>
  <Card>
    <CardHeader><CardTitle>{{ t('eq_title') }}</CardTitle></CardHeader>
    <CardContent v-if="state" class="flex flex-col gap-4">
      <div class="flex flex-wrap items-center gap-x-6 gap-y-2">
        <div class="flex items-center gap-2">
          <Switch
            id="eq-enabled" data-eq-enabled
            :model-value="state.enabled"
            @update:model-value="setEnabled"
          />
          <Label for="eq-enabled">{{ t('eq_enabled') }}</Label>
        </div>
        <div class="flex items-center gap-2">
          <Switch
            id="eq-auto-preamp" data-eq-auto-preamp
            :disabled="!state.enabled"
            :model-value="state.auto_preamp"
            @update:model-value="setAutoPreamp"
          />
          <Label for="eq-auto-preamp" :class="{ 'opacity-50': !state.enabled }">{{ t('eq_auto_preamp') }}</Label>
        </div>
      </div>

      <div class="flex flex-wrap items-center gap-2">
        <Select
          :model-value="currentPreset ?? CUSTOM"
          @update:model-value="pickPreset"
        >
          <SelectTrigger data-eq-preset class="min-w-48" :aria-label="t('eq_preset')">
            <SelectValue>{{ presetLabel }}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem v-for="b in builtins" :key="b.id" :value="`builtin:${b.id}`">{{ b.name }}</SelectItem>
            <SelectItem v-for="p in state.user_presets" :key="p.name" :value="userKey(p.name)">{{ p.name }}</SelectItem>
          </SelectContent>
        </Select>
        <Button variant="outline" data-eq-save @click="openSave">{{ t('eq_save_preset') }}</Button>
        <Button variant="outline" data-eq-delete :disabled="!deletable" @click="openDelete">
          {{ t('eq_delete_preset') }}
        </Button>
        <Button variant="outline" data-eq-flat @click="resetFlat">{{ t('eq_reset_flat') }}</Button>
      </div>

      <!-- Phone: ten equal columns sharing the card's width (24 px each at
           the 240 px floor; ≈ 26 px in the ≈ 260 px a 360 px screen leaves),
           each slider narrowed to a 24 px wide hit area so neighbouring
           thumbs never overlap. From `sm:` up, the fixed 44 px columns of the
           desktop layout. `overflow-x-auto` is only the fallback under the
           240 px floor (viewports narrower than ≈ 320 px): the bands then
           scroll inside the card, never the page. -->
      <div data-eq-bands class="overflow-x-auto">
        <div data-eq-band-row class="grid min-w-60 grid-cols-10 sm:flex sm:min-w-max sm:justify-between sm:gap-1">
          <div
            v-for="(hz, i) in bandsHz" :key="hz"
            data-eq-band
            class="flex min-w-0 flex-col items-center gap-1 sm:w-11"
          >
            <Slider
              class="h-36 data-[orientation=vertical]:px-[9px] sm:data-[orientation=vertical]:px-[19px]"
              orientation="vertical"
              :min="-12" :max="12" :step="1"
              :disabled="!state.enabled"
              :model-value="[state.gains[i] ?? 0]"
              :aria-label="`${hz} Hz`"
              @update:model-value="(v: number[] | undefined) => setGain(i, v)"
            />
            <span data-eq-band-label class="text-xs text-muted-foreground">{{ bandLabel(hz) }}</span>
            <span data-eq-band-value class="text-xs tabular-nums">{{ signed(state.gains[i] ?? 0) }}</span>
          </div>
        </div>
      </div>

      <p data-eq-hint class="text-sm text-muted-foreground">
        {{ state.auto_preamp ? t('eq_hint_auto') : t('eq_hint_manual') }}
      </p>
    </CardContent>
  </Card>

  <Dialog :open="naming" @update:open="(v: boolean) => { naming = v }">
    <DialogContent data-eq-name-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('eq_preset_name') }}</DialogTitle>
      </DialogHeader>
      <form class="flex flex-col gap-3" @submit.prevent="submitName">
        <Input
          v-model="name" data-eq-name maxlength="40" :aria-label="t('eq_preset_name')"
          @update:model-value="overwriting = null"
        />
        <p v-if="overwriting !== null" data-eq-overwrite class="text-sm">
          {{ t('eq_overwrite_confirm', { name: overwriting }) }}
        </p>
        <div class="flex justify-end gap-2">
          <Button
            v-if="overwriting !== null" type="button" variant="destructive" data-eq-overwrite-confirm
            @click="save(trimmedName)"
          >
            {{ t('eq_overwrite') }}
          </Button>
          <Button v-else type="submit" data-eq-name-submit :disabled="trimmedName === ''">{{ t('save') }}</Button>
        </div>
      </form>
    </DialogContent>
  </Dialog>

  <Dialog :open="deleting" @update:open="(v: boolean) => { deleting = v }">
    <DialogContent data-eq-delete-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('eq_delete_preset') }}</DialogTitle>
        <DialogDescription>
          {{ t('eq_delete_confirm', { name: deleteName }) }}
        </DialogDescription>
      </DialogHeader>
      <div class="flex justify-end">
        <Button variant="destructive" data-eq-delete-confirm @click="confirmDelete">{{ t('eq_delete_preset') }}</Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
