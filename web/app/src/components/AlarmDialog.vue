<script setup lang="ts">
/**
 * Creating or editing one alarm. The dialog edits a **draft** (`cloneAlarm`)
 * and hands the finished alarm back through `save`: nothing reaches the list,
 * nor the core, before that — closing the dialog throws the draft away.
 * `AlarmList` decides what a save or a delete does to the list, and
 * `ConfigView` writes it.
 *
 * Save is refused with no day ticked: a one-shot alarm is out of scope, and
 * the core refuses an empty `days` too (`settings_alarm_no_days`). The other
 * bounds mirror the core's (`settings_validation.rs`) so a refusal is seen
 * here rather than as a toast after the round trip.
 *
 * The device's own time is the alarm's time, with no zone shown or asked: the
 * device already has what it needs to set its clock, and shows it in standby.
 */
import {
  Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, Input, Label,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Slider, Switch,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { Alarm, AlarmTarget, SleepMode, Weekday } from '../types'
import {
  ALARM_SLEEP_MAX, ALARM_SLEEP_MIN, cloneAlarm, DEFAULT_SLEEP_AFTER_MIN, shortDayLabel, WEEK,
} from './alarms'
import { SLEEP_MODES } from './sleepTimer'

const props = defineProps<{
  open: boolean
  /** The alarm the dialog opens on: an existing one, or `newAlarm(...)`. */
  alarm: Alarm
  /** A new alarm has nothing to delete. */
  isNew: boolean
  /** The installed sources, by plugin name, in the page's order. */
  sources: string[]
  /** The device's 12/24 h setting (`SettingsPayload.clock_24h`). */
  clock24h: boolean
  /** A save is in flight: the buttons wait for it. */
  busy?: boolean
}>()
const emit = defineEmits<{ 'update:open': [boolean]; save: [Alarm]; delete: [number] }>()
const { t } = useCatalog()

const RESUME = 'resume'
const SOURCE_PREFIX = 'source:'

const hour = ref(0)
// A string once the field is edited (`Input` binds the native value): read
// through `Number` below, never trusted as a number.
const minute = ref<string | number>(0)
const days = ref<Weekday[]>([])
const target = ref(RESUME)
const volume = ref(0)
const sleepOn = ref(false)
const sleepMinutes = ref<string | number>(DEFAULT_SLEEP_AFTER_MIN)
const sleepMode = ref<SleepMode>('fade')

function load() {
  const a = cloneAlarm(props.alarm)
  hour.value = a.hour
  minute.value = a.minute
  days.value = [...a.days]
  target.value = a.target.kind === 'source' ? SOURCE_PREFIX + a.target.name : RESUME
  volume.value = a.volume
  sleepOn.value = a.sleep_after !== undefined
  sleepMinutes.value = a.sleep_after?.minutes ?? DEFAULT_SLEEP_AFTER_MIN
  sleepMode.value = a.sleep_after?.mode ?? 'fade'
}
// Reloaded on each opening, so a dialog closed half-edited reopens on the
// saved alarm and not on the abandoned draft.
watch(() => [props.open, props.alarm] as const, ([open]) => { if (open) load() }, { immediate: true })

/** The source an alarm still names is offered even once uninstalled, so
 * opening the alarm does not silently retarget it. */
const sourceChoices = computed(() => {
  const names = [...props.sources]
  const a = props.alarm.target
  if (a.kind === 'source' && !names.includes(a.name)) names.push(a.name)
  return names
})

function hourLabel(h: number): string {
  if (props.clock24h) return String(h).padStart(2, '0')
  return `${h % 12 === 0 ? 12 : h % 12} ${h < 12 ? 'AM' : 'PM'}`
}

function dayLabel(d: Weekday): string {
  switch (d) {
    case 'monday': return t.value('weekday_monday')
    case 'tuesday': return t.value('weekday_tuesday')
    case 'wednesday': return t.value('weekday_wednesday')
    case 'thursday': return t.value('weekday_thursday')
    case 'friday': return t.value('weekday_friday')
    case 'saturday': return t.value('weekday_saturday')
    default: return t.value('weekday_sunday')
  }
}

function modeLabel(m: SleepMode): string {
  switch (m) {
    case 'immediate': return t.value('sleep_timer_mode_immediate')
    case 'fade': return t.value('sleep_timer_mode_fade')
    default: return t.value('sleep_timer_mode_end_of_track')
  }
}

function toggleDay(d: Weekday) {
  days.value = days.value.includes(d) ? days.value.filter((x) => x !== d) : [...days.value, d]
}

function isInt(v: string | number, min: number, max: number): boolean {
  if (typeof v === 'string' && v.trim() === '') return false
  const n = Number(v)
  return Number.isInteger(n) && n >= min && n <= max
}

const noDays = computed(() => days.value.length === 0)
const minuteValid = computed(() => isInt(minute.value, 0, 59))
const sleepValid = computed(() => !sleepOn.value || isInt(sleepMinutes.value, ALARM_SLEEP_MIN, ALARM_SLEEP_MAX))
const canSave = computed(() => !noDays.value && minuteValid.value && sleepValid.value && !props.busy)

function save() {
  if (!canSave.value) return
  const chosen: AlarmTarget = target.value.startsWith(SOURCE_PREFIX)
    ? { kind: 'source', name: target.value.slice(SOURCE_PREFIX.length) }
    : { kind: 'resume' }
  const out: Alarm = {
    id: props.alarm.id,
    enabled: props.alarm.enabled,
    hour: hour.value,
    minute: Number(minute.value),
    // Week order, so the saved list reads the same as the toggles.
    days: WEEK.filter((d) => days.value.includes(d)),
    target: chosen,
    volume: volume.value,
  }
  // Absent, not `null`, when off: the wire's `skip_serializing_if`.
  if (sleepOn.value) out.sleep_after = { minutes: Number(sleepMinutes.value), mode: sleepMode.value }
  emit('save', out)
}
</script>

<template>
  <Dialog :open="open" @update:open="(v: boolean) => emit('update:open', v)">
    <DialogContent data-alarm-dialog>
      <DialogHeader>
        <DialogTitle>{{ isNew ? t('alarm_new_title') : t('alarm_edit_title') }}</DialogTitle>
        <DialogDescription>{{ t('alarm_description') }}</DialogDescription>
      </DialogHeader>

      <div class="grid gap-4">
        <div class="grid gap-2">
          <span id="alarm-time" class="text-sm font-medium">{{ t('alarm_time') }}</span>
          <div role="group" aria-labelledby="alarm-time" class="flex items-center gap-2">
            <Select
              :model-value="String(hour)"
              @update:model-value="(v) => (hour = Number(v))"
            >
              <SelectTrigger class="w-24" data-alarm-hour :aria-label="t('alarm_hour')">
                <SelectValue>{{ hourLabel(hour) }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem v-for="h in 24" :key="h - 1" :value="String(h - 1)">{{ hourLabel(h - 1) }}</SelectItem>
              </SelectContent>
            </Select>
            <span aria-hidden="true">:</span>
            <Input
              v-model="minute"
              data-alarm-minute
              type="number" min="0" max="59" step="1" class="w-20"
              :aria-label="t('alarm_minute')"
              :aria-invalid="minuteValid ? undefined : 'true'"
            />
          </div>
        </div>

        <div class="grid gap-2">
          <span id="alarm-days" class="text-sm font-medium">{{ t('alarm_days') }}</span>
          <div role="group" aria-labelledby="alarm-days" class="flex flex-wrap gap-1">
            <Button
              v-for="d in WEEK"
              :key="d"
              :data-alarm-day="d"
              size="sm"
              :variant="days.includes(d) ? 'default' : 'outline'"
              :aria-pressed="days.includes(d) ? 'true' : 'false'"
              :aria-label="dayLabel(d)"
              @click="toggleDay(d)"
            >{{ shortDayLabel(t, d) }}</Button>
          </div>
          <p v-if="noDays" data-alarm-no-days class="text-xs text-destructive">{{ t('alarm_no_days') }}</p>
        </div>

        <div class="grid gap-2">
          <Label for="alarm-target">{{ t('alarm_target') }}</Label>
          <Select v-model="target">
            <SelectTrigger id="alarm-target" class="min-w-40" data-alarm-target>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem :value="RESUME">{{ t('alarm_target_resume') }}</SelectItem>
              <SelectItem v-for="s in sourceChoices" :key="s" :value="SOURCE_PREFIX + s">{{ s }}</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div class="grid gap-2">
          <span id="alarm-volume" class="text-sm font-medium">{{ t('volume') }} — <span data-alarm-volume-value>{{ volume }}</span></span>
          <Slider
            data-alarm-volume
            :model-value="[volume]"
            :min="0" :max="100" :step="1"
            :aria-label="t('volume')"
            @update:model-value="(v?: number[]) => { if (v && v[0] !== undefined) volume = v[0] }"
          />
        </div>

        <div class="grid gap-2">
          <div class="flex items-center gap-2">
            <Switch id="alarm-sleep" data-alarm-sleep v-model="sleepOn" />
            <Label for="alarm-sleep">{{ t('alarm_sleep_after') }}</Label>
          </div>
          <template v-if="sleepOn">
            <div class="flex items-center gap-2">
              <Input
                v-model="sleepMinutes"
                data-alarm-sleep-minutes
                type="number" :min="ALARM_SLEEP_MIN" :max="ALARM_SLEEP_MAX" step="1" class="w-24"
                :aria-label="t('alarm_sleep_minutes')"
                :aria-invalid="sleepValid ? undefined : 'true'"
              />
              <span class="text-sm">{{ t('alarm_sleep_minutes') }}</span>
            </div>
            <div role="radiogroup" :aria-label="t('sleep_timer_when')" class="flex flex-wrap gap-2">
              <Button
                v-for="m in SLEEP_MODES"
                :key="m"
                role="radio"
                size="sm"
                :data-alarm-sleep-mode="m"
                :aria-checked="sleepMode === m ? 'true' : 'false'"
                :variant="sleepMode === m ? 'default' : 'outline'"
                @click="sleepMode = m"
              >{{ modeLabel(m) }}</Button>
            </div>
          </template>
        </div>
      </div>

      <div class="flex flex-wrap justify-between gap-2">
        <Button
          v-if="!isNew"
          variant="destructive"
          data-alarm-delete
          :disabled="busy"
          @click="emit('delete', alarm.id)"
        >{{ t('alarm_delete') }}</Button>
        <Button class="ml-auto" data-alarm-save :disabled="!canSave" @click="save">{{ t('save') }}</Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
