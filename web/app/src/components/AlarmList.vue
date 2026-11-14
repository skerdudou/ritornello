<script setup lang="ts">
/**
 * The Alarms card's body: one line per alarm, phone-style — the time, the
 * days as chips, what it plays, and an on/off switch right on the line. The
 * line itself opens `AlarmDialog`; so does "Add", on a fresh alarm.
 *
 * Every gesture builds the **whole** next list and hands it to `persist`
 * (`ConfigView`, which PUTs the full settings): the list on screen is the
 * `alarms` prop, i.e. what the core accepted, so a refused write leaves the
 * line as it was — a switch flipped on a refused PUT flips back by itself.
 * The dialog closes only on an accepted write; on a refusal it stays open on
 * the draft, the error shown by `persist` the way the page shows the others.
 */
import { Badge, Button, Switch } from '@ritornello/ui'
import { computed, ref } from 'vue'
import { formatHourMinute } from '../composables/timezone'
import { useCatalog } from '../composables/useCatalog'
import type { Alarm } from '../types'
import AlarmDialog from './AlarmDialog.vue'
import { cloneAlarm, daysInWeekOrder, newAlarm, shortDayLabel } from './alarms'

const props = defineProps<{
  alarms: Alarm[]
  /** The installed sources, by plugin name. */
  sources: string[]
  clock24h: boolean
  /** Writes the complete next list; resolves `true` once the core accepted it. */
  persist: (alarms: Alarm[]) => Promise<boolean>
}>()
const { t } = useCatalog()

const busy = ref(false)
const dialogOpen = ref(false)
const editing = ref<Alarm>(newAlarm([]))
const editingIsNew = ref(true)

/** In time order, as on a phone; the id breaks a tie so the order is stable. */
const sorted = computed(() =>
  [...props.alarms].sort((a, b) => a.hour * 60 + a.minute - (b.hour * 60 + b.minute) || a.id - b.id),
)

function timeLabel(a: Alarm): string {
  return formatHourMinute(a.hour + a.minute / 60, props.clock24h)
}

function targetLabel(a: Alarm): string {
  return a.target.kind === 'source' ? a.target.name : t.value('alarm_target_resume')
}

async function write(next: Alarm[]): Promise<boolean> {
  busy.value = true
  try {
    return await props.persist(next)
  } finally {
    busy.value = false
  }
}

function openNew() {
  editing.value = newAlarm(props.alarms)
  editingIsNew.value = true
  dialogOpen.value = true
}

function openExisting(a: Alarm) {
  editing.value = cloneAlarm(a)
  editingIsNew.value = false
  dialogOpen.value = true
}

/** Only this alarm's `enabled` changes; every other alarm goes back as read. */
async function toggle(a: Alarm, enabled: boolean) {
  await write(props.alarms.map((x) => (x.id === a.id ? { ...cloneAlarm(x), enabled } : cloneAlarm(x))))
}

async function onSave(alarm: Alarm) {
  const next = editingIsNew.value
    ? [...props.alarms.map(cloneAlarm), alarm]
    : props.alarms.map((x) => (x.id === alarm.id ? alarm : cloneAlarm(x)))
  if (await write(next)) dialogOpen.value = false
}

async function onDelete(id: number) {
  if (await write(props.alarms.filter((x) => x.id !== id).map(cloneAlarm))) dialogOpen.value = false
}
</script>

<template>
  <div class="flex flex-col gap-3">
    <p v-if="alarms.length === 0" data-alarms-empty class="text-sm text-muted-foreground">{{ t('alarms_empty') }}</p>
    <ul v-else class="divide-y" data-alarm-lines>
      <li
        v-for="a in sorted"
        :key="a.id"
        :data-alarm-line="a.id"
        class="flex items-center gap-3 py-2"
      >
        <button
          type="button"
          class="flex min-w-0 flex-1 flex-col items-start gap-1 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
          :class="a.enabled ? '' : 'text-muted-foreground'"
          :data-alarm-open="a.id"
          :aria-label="t('alarm_edit_label', { time: timeLabel(a) })"
          @click="openExisting(a)"
        >
          <span class="text-2xl font-medium tabular-nums" data-alarm-time>{{ timeLabel(a) }}</span>
          <span class="flex flex-wrap items-center gap-1">
            <Badge
              v-for="d in daysInWeekOrder(a.days)"
              :key="d"
              variant="secondary"
              :data-alarm-chip="d"
            >{{ shortDayLabel(t, d) }}</Badge>
            <span class="text-xs text-muted-foreground" data-alarm-target-label>{{ targetLabel(a) }}</span>
          </span>
        </button>
        <Switch
          :data-alarm-enabled="a.id"
          :model-value="a.enabled"
          :disabled="busy"
          :aria-label="t('alarm_enabled_label', { time: timeLabel(a) })"
          @update:model-value="(v: boolean) => toggle(a, v)"
        />
      </li>
    </ul>
    <div>
      <Button variant="outline" data-alarm-add :disabled="busy" @click="openNew">{{ t('alarms_add') }}</Button>
    </div>
    <AlarmDialog
      :open="dialogOpen"
      :alarm="editing"
      :is-new="editingIsNew"
      :sources="sources"
      :clock24h="clock24h"
      :busy="busy"
      @update:open="(v: boolean) => (dialogOpen = v)"
      @save="onSave"
      @delete="onDelete"
    />
  </div>
</template>
