<script setup lang="ts">
import {
  api, Button, createT, Input, type Catalog,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import LearnDialog from './LearnDialog.vue'
import {
  ACTIONS, codesFor, collect, conflicts, parseField, presetToml, sanitiseDeviceName,
  type BindingTable, type Conflict,
} from './preset-toml'

// `base` is part of the plugin UI contract, same as `catalog`: the
// **absolute** prefix under which the core serves this plugin's routes
// (`/plugins/generic-input/`), provided by the shell.
//
// This view used to call `api.get('./api/data')` relatively — so resolved
// against the browser's URL, not against anything the contract guarantees.
// On `/plugins/generic-input` (without a trailing slash, a form the shell's
// router also accepted), `./api/data` resolved to `/plugins/api/data`,
// which the core interprets as the "api" plugin: 404, empty table, load
// error and every button failing.
//
// **Required** prop, with no default value: the name under which this
// plugin is served comes from `plugins.toml`, i.e. from deployment, not
// from this file. A default of `/plugins/generic-input/` would be wrong as
// soon as the operator declares this plugin under another name, and would
// be wrong *silently*. Better for the shell to be required to provide the
// prefix — which a `PluginView` test checks.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** Absolute URL of a route of this plugin, built from `base`. */
function url(path: string): string {
  return `${props.base}${path}`
}

const PROBE_MS = 300
const TIMEOUT_MS = 30_000

interface Data {
  devices: string[]
  bindings: BindingTable
  presets: string[]
  learning: { captured: number | null } | null
}

const data = ref<Data>({ devices: [], bindings: { devices: [] }, presets: [], learning: null })
const device = ref('')
const preset = ref('')
const codes = ref<string[]>(ACTIONS.map(() => ''))
const message = ref('')
// Row (index into `ACTIONS`) whose key is being learned, `null` otherwise:
// the sole source of truth for the "learning in progress" state, it drives
// the opening of the popup. It is not a write target: the destination of
// the captured code is `i`, captured by `learn`'s closure.
const learnedRow = ref<number | null>(null)
/**
 * Seconds remaining before abandonment, for the popup.
 *
 * Computed here and not in the popup: the deadline lives with the probing,
 * and a second timer on the popup side would drift from the first -- it
 * would display a figure nothing guarantees. Zero means "no learning in
 * progress".
 */
const secondsLeft = ref(0)
// "Add to existing codes" checkbox of the popup, reset to false on every
// opening: the default gesture stays replacement.
const add = ref(false)
/** Translated label of the learned action, for the popup's title. */
const learnedActionLabel = computed(() => {
  const i = learnedRow.value
  const key = i === null ? undefined : ACTIONS[i]?.key
  return key ? t.value(key) : ''
})
let timer: ReturnType<typeof setInterval> | null = null
// Synchronous guard against the race described in review (round 1): `timer`
// is only assigned after the `learn` PUT's `await`, so a second trigger
// (double-click, or click on another row) while this PUT is in flight would
// see `timer` still null and would also pass the guard based on `timer`
// alone -- resulting in two `setInterval`s, the second overwriting the
// reference to the first, which becomes orphaned (never `clearInterval`d)
// and can write a captured code into the wrong action. This flag is set
// before any `await`, so it takes effect immediately.
let learnInFlight = false

function fillCodes() {
  codes.value = ACTIONS.map((a) => (device.value ? codesFor(data.value.bindings, device.value, a.cmd) : ''))
}

async function reload() {
  try {
    data.value = await api.get<Data>(url('api/data'))
    if (!data.value.devices.includes(device.value)) device.value = data.value.devices[0] ?? ''
    // Same handling as `device`: a selected preset that disappears from the
    // list (e.g. the shipped file being removed) would otherwise leave the
    // `Select` pointing at a value with no matching `SelectItem`.
    if (!data.value.presets.includes(preset.value)) preset.value = data.value.presets[0] ?? ''
    fillCodes()
    message.value = device.value ? '' : t.value('no_device')
  } catch (e) {
    message.value = t.value('load_error') + (e as Error).message
  }
}

onMounted(reload)
onUnmounted(() => stopTimer())

// Changing device cancels the ongoing learning session **before**
// repopulating the table, like the old handler used to
// (`$('dev').onchange = async () => { if (timer) await stopLearn(''); … }`).
//
// Without this cancellation, the interval keeps probing while the server's
// learning session is still armed on the **previous** device;
// `fillCodes()` has in the meantime repopulated the table from the
// **new** device's bindings, so the closure writes the captured code into
// the new device's row, and "Save" persists it — a key assigned to the
// wrong device. The UI would also stay in the "press a key" state for a
// device nobody is learning anymore.
//
// `stopLearn` calls `stopTimer()` synchronously before any `await`: the
// interval is therefore dead before the `cancel_learn` PUT leaves, and no
// probe can slip in during the round trip.
//
// `stopLearn` does an `await fetch` (PUT `cancel_learn`) that can reject
// (network cut off): without `try`/`finally`, the uncaught rejection would
// skip `fillCodes()`, and the **previous** device's codes would stay
// displayed under the new one -- exactly the class of bug this watcher
// fixes, in the network-failure branch.
watch(device, async () => {
  try {
    if (timer) await stopLearn('')
  } catch {
    // Best-effort: a failed network cancellation must not prevent
    // repopulating the table for the new device (see comment above).
  } finally {
    fillCodes()
  }
})

function stopTimer() {
  if (timer) clearInterval(timer)
  timer = null
  // Reset to zero along with the timer that feeds it: otherwise the last
  // displayed figure would stay frozen behind the overlay, and would
  // reappear as-is on the next opening, before the first probing round.
  secondsLeft.value = 0
}

async function stopLearn(text: string) {
  stopTimer()
  // Before any `await`, like `stopTimer()`: the popup closes right at the
  // gesture (cancellation, capture, device change), not at the end of the
  // network round trip -- which can moreover fail.
  learnedRow.value = null
  await api.put(url('api/data'), { op: 'cancel_learn' })
  message.value = text
}

/**
 * Cancellation requested by the popup — button, kit close icon, Escape,
 * click on the overlay: four gestures, a single path, and a named function
 * rather than an async call written into the template.
 *
 * The promise is explicitly abandoned (`void`) and its failure swallowed.
 * Nothing the user sees depends on it: `stopLearn` stops the timer and
 * closes the popup **before** any `await`. A network failure does not even
 * reject here -- `api.put` converts it into a return value (see
 * `web/kit/src/api.ts`, precisely so that no caller needs a `try`); the
 * `catch` is a belt-and-braces for the day `stopLearn` gains a step that
 * throws, since a template handler's promise has nowhere to be awaited.
 */
function cancelLearn() {
  void stopLearn('').catch(() => {})
}

/** Field updated for a captured code: appended to the list, or replaced. */
function applyCode(field: string, code: number, append: boolean): string {
  if (!append || !field.trim()) return String(code)
  // Comparison on parsed codes, not on the text: `' 9 '` does carry code 9.
  // A `9, 9` would be rejected at save time anyway (`duplicate_code`), and
  // the user hasn't asked for anything more.
  if (parseField(field).includes(code)) return field
  // The field is kept exactly as written, spaces included: the user typed
  // what they typed.
  return `${field}, ${code}`
}

// Learning: the plugin captures the device's next key, the view probes
// `GetData` until it sees it arrive. Same mechanics as the old page —
// short probing, explicit cancellation — but a 30 s timeout instead of
// 10: the time to find the right key on an unfamiliar remote.
async function learn(i: number) {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  // Silently give up if a previous trigger is already in flight: the first
  // one will succeed at establishing a valid learning session anyway, and
  // letting the second one proceed would produce a second concurrent
  // `setInterval` (see the comment on `learnInFlight`).
  if (learnInFlight) return
  learnInFlight = true
  try {
    if (timer) await stopLearn('')
    const err = await api.put(url('api/data'), { op: 'learn', device: device.value })
    if (err) {
      message.value = err
      return
    }
    // Belt and braces: in case a timer got (re)installed between the start
    // of this function and here, `timer` is never replaced without having
    // explicitly stopped the old one.
    stopTimer()
    // The "press a key" instruction is carried by the popup, which names
    // the action and the device: nothing to write in the bottom bar, which
    // the overlay covers anyway.
    learnedRow.value = i
    // ... but it must be cleared: otherwise the previous session's "Timed
    // out" would still linger there behind the overlay while a fresh popup
    // waits for a press.
    message.value = ''
    add.value = false
    const deadline = Date.now() + TIMEOUT_MS
    // Set right now, before the timer's first tick: otherwise the popup
    // would open on an empty countdown for the duration of one tick.
    secondsLeft.value = Math.ceil(TIMEOUT_MS / 1000)
    // Overlap guard: on a slow machine, a GET that exceeds the interval
    // would stack requests in the plugin's serial queue — the same risk
    // documented for the radio search.
    let probeInFlight = false
    timer = setInterval(async () => {
      if (probeInFlight) return
      probeInFlight = true
      try {
        if (Date.now() > deadline) {
          await stopLearn(t.value('learn_timeout'))
          return
        }
        // Rounded up: at 29.4 s remaining we display "30", never a
        // misleading "0" on the last fraction of a second -- abandonment
        // itself is decided by the comparison above, not by this figure.
        secondsLeft.value = Math.ceil((deadline - Date.now()) / 1000)
        let d: Data
        try {
          d = await api.get<Data>(url('api/data'))
        } catch {
          return // a failed read must not interrupt probing
        }
        const c = d.learning?.captured
        if (c !== null && c !== undefined) {
          codes.value[i] = applyCode(codes.value[i] ?? '', c, add.value)
          await stopLearn('')
        }
      } finally {
        probeInFlight = false
      }
    }, PROBE_MS)
  } finally {
    learnInFlight = false
  }
}

// Live validation of duplicate assignments: recomputed on every keystroke,
// `codes` being an array `ref` bound by `v-model` — a code arriving via
// learning also flows through it, since `applyCode` writes into this same
// array.
const conflictsByAction = computed(() => conflicts(codes.value))
const hasConflicts = computed(() => conflictsByAction.value.some((c) => c !== null))

/** Sentence displayed below a faulty field. */
function conflictText(c: Conflict): string {
  if (c.others.length) {
    // The **translated** labels of the other actions, never their i18n keys.
    return t.value('conflict_code', { code: c.code, action: c.others.map((k) => t.value(k)).join(', ') })
  }
  return t.value('conflict_dup', { code: c.code })
}

// No guard on `hasConflicts` here: the disabled button is the only call
// path, and restating the rule in the function would create two truths to
// maintain. The server would reject the whole table anyway
// (`duplicate_code`).
async function save() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const table = collect(data.value.bindings, device.value, codes.value)
  const err = await api.put(url('api/data'), { op: 'save', bindings: table })
  if (err) {
    message.value = t.value('save_error') + err
    return
  }
  data.value.bindings = table
  message.value = t.value('saved')
}

async function refresh() {
  await api.put(url('api/data'), { op: 'rescan' })
  await reload()
}

async function loadPreset() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const err = await api.put(url('api/data'), {
    op: 'load_preset',
    device: device.value,
    preset: preset.value,
  })
  if (err) {
    message.value = err
    return
  }
  await reload()
}

// Reads a file as text via `FileReader` rather than `Blob.text()`: the
// latter is not implemented by jsdom (the tests' environment), while
// `FileReader` works there, as in any real browser.
function readTextFile(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(String(reader.result ?? ''))
    reader.onerror = () => reject(reader.error ?? new Error('unable to read file'))
    reader.readAsText(file)
  })
}

// Import: the file is read as text on the browser side, then parsed and
// validated on the server side (`import_preset`) — exactly like
// `load_preset` but without going through /etc/ritornello/input-presets.
// Nothing is persisted before an explicit "Save".
async function import_(e: Event) {
  const input = e.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = '' // allows re-importing the same file afterwards
  if (!file) return
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  try {
    const content = await readTextFile(file)
    const err = await api.put(url('api/data'), {
      op: 'import_preset',
      device: device.value,
      content,
    })
    if (err) {
      message.value = err
      return
    }
    await reload()
  } catch (err) {
    message.value = t.value('load_error') + (err as Error).message
  }
}

function export_() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const d = data.value.bindings.devices.find((x) => x.name === device.value)
  const blob = new Blob([presetToml(d ? d.bindings : [])], { type: 'application/toml' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `ritornello-bindings-${sanitiseDeviceName(device.value)}.toml`
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center gap-2">
      <!-- The neighboring <label> is not associated with the trigger (no
           for/id through the Select component): the aria-label provides the
           accessible name. -->
      <label class="text-sm text-muted-foreground">{{ t('device_label') }}</label>
      <Select v-model="device">
        <SelectTrigger data-device-select class="min-w-64" :aria-label="t('device_label')"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="d in data.devices" :key="d" :value="d">{{ d }}</SelectItem>
        </SelectContent>
      </Select>
      <Button variant="secondary" data-refresh @click="refresh">{{ t('btn_refresh') }}</Button>
    </div>

    <table class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="text-left font-normal">{{ t('col_action') }}</th>
          <th class="text-left font-normal">{{ t('col_code') }}</th>
          <th class="w-24" /><th class="w-24" />
        </tr>
      </thead>
      <tbody>
        <tr v-for="(a, i) in ACTIONS" :key="a.key" data-action-row class="border-t border-border">
          <td class="py-1">{{ t(a.key) }}</td>
          <td class="py-1 pr-2">
            <!-- No red class to add: the kit's `Input` already carries
                 `aria-invalid:border-destructive` and the red ring. Setting
                 the attribute is the whole signal. -->
            <Input v-model="codes[i]" inputmode="numeric" :aria-invalid="!!conflictsByAction[i]" />
            <p v-if="conflictsByAction[i]" data-conflict class="mt-1 text-xs text-destructive">
              {{ conflictText(conflictsByAction[i]!) }}
            </p>
          </td>
          <td><Button variant="secondary" size="sm" data-learn @click="learn(i)">{{ t('btn_learn') }}</Button></td>
          <td><Button variant="ghost" size="sm" data-clear @click="codes[i] = ''">{{ t('btn_clear') }}</Button></td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <label class="text-sm text-muted-foreground">{{ t('preset_label') }}</label>
      <Select v-model="preset">
        <SelectTrigger class="min-w-40" :aria-label="t('preset_label')"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="p in data.presets" :key="p" :value="p">{{ p }}</SelectItem>
        </SelectContent>
      </Select>
      <Button variant="secondary" @click="loadPreset">{{ t('btn_load_preset') }}</Button>
      <label class="cursor-pointer rounded-md border border-border px-3 py-2 text-sm">
        {{ t('btn_import') }}
        <input type="file" accept=".toml" data-import class="hidden" @change="import_" />
      </label>
      <Button variant="secondary" @click="export_">{{ t('btn_export') }}</Button>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <Button data-save :disabled="hasConflicts" @click="save">{{ t('btn_save') }}</Button>
      <span v-if="hasConflicts" data-save-blocked class="text-sm text-destructive">{{ t('save_conflicts') }}</span>
      <span class="text-sm text-muted-foreground">{{ message }}</span>
    </div>

    <!-- Cancellation now lives in the popup: a button left in the bar above
         would end up behind the overlay, hence unreachable. -->
    <LearnDialog
      :open="learnedRow !== null"
      :t="t"
      :action="learnedActionLabel"
      :device="device"
      :seconds="secondsLeft"
      v-model:add="add"
      @cancel="cancelLearn"
    />
  </div>
</template>
