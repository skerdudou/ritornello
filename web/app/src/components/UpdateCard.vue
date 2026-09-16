<script setup lang="ts">
import {
  Button, Card, CardContent, CardHeader, CardTitle,
  Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch,
} from '@ritornello/ui'
import { computed } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { SettingsPayload, UpdatePayload, Weekday } from '../types'
import UpdateSummary from './UpdateSummary.vue'

/**
 * The device's own view of itself against the last release it read: one
 * line, an error or a rollback note when there is one, and the two gestures
 * — check, install — plus, below a separator, the automatic-checks policy
 * that decides what the device does on its own.
 *
 * **No network call of its own.** It takes its update payload and the page's
 * shared `settings` object as props — the same reference the page reads and
 * writes elsewhere, mutated in place rather than copied — and emits
 * `check`/`install`/`save`; the page that mounts it owns `GET /api/update`,
 * the two update `POST`s, and the one `PUT /api/settings`. This is what
 * makes the card testable without stubbing `fetch` — the same shape as
 * `CoverCacheDetails.vue`'s panel, one level up (that one reads its own
 * snapshot; this one is handed one because two different buttons on the same
 * page — Check and Install — must react to the very same payload without
 * racing each other's reload).
 */
const props = defineProps<{ update: UpdatePayload; settings: SettingsPayload }>()
const emit = defineEmits<{ check: []; install: []; save: [] }>()
const { t } = useCatalog()

const WEEKDAYS: Weekday[] = [
  'sunday', 'monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday',
]

/** Components an install would act on: everything out of step, core included. */
const actionable = computed(() =>
  props.update.components.filter((c) => c.availability === 'update_available'),
)

/**
 * Read through a `computed` rather than in the template: narrowing a
 * discriminated union across a template `v-if` is not something `vue-tsc`
 * can be trusted to carry into a sibling expression, where this file's own
 * `outcome.kind === 'failed'` check would otherwise need `outcome.detail`
 * read a second time.
 */
const errorDetail = computed(() =>
  props.update.outcome.kind === 'failed' ? props.update.outcome.detail : null,
)

/**
 * The transient report of a just-finished install (`CheckOutcome::Installed`,
 * Ruling 55). Shown alongside `UpdateSummary` (the summary line moved to its
 * own component in Task 5), never instead of it: the row itself flipping to
 * "up to date" is the durable signal, and this is only the one-off sentence
 * naming what just happened.
 */
const installedDetail = computed(() =>
  props.update.outcome.kind === 'installed' ? props.update.outcome.detail : null,
)

/**
 * What the core's own last-installed archive did not touch — the privileged
 * installer, the systemd units, the polkit rules `install_one` never places.
 * `undefined` until a core install has actually happened, and never an empty
 * array once it has: the core's archive always carries something here (see
 * `archive::core_not_installed`'s own doc comment).
 */
const coreArchiveNoteCount = computed(() => {
  const core = props.update.components.find((c) => c.kind === 'core')
  return core?.not_installed_files?.length ?? 0
})

/**
 * The rollback sentence, and **which** of the two it is.
 *
 * `update_rolled_back` says "the previous version was put back", and a
 * rollback that restored nothing used to say exactly that. The rollback unit
 * writes a report with an empty `restored` and a populated `failed` when the
 * backup manifest is corrupt, and then deliberately does not restart the
 * service — so the device is down, and when the operator brings it back by
 * hand the card told them the previous version was in place. A failure
 * reported as a success, in the one place a nocturnal rollback is ever
 * mentioned.
 *
 * Anything less than a clean rollback takes the other sentence: nothing
 * restored, or something restored and something else not. `null` when there is
 * no report, which is the ordinary device.
 */
const rollbackNote = computed(() => {
  const report = props.update.last_rollback
  if (!report) return null
  const clean = report.restored.length > 0 && report.failed.length === 0
  return t.value(clean ? 'update_rolled_back' : 'update_rollback_failed')
})

const updatePolicyLabel = computed(() => {
  switch (props.settings.update_policy) {
    case 'check':
      return t.value('update_policy_check')
    case 'check_and_install':
      return t.value('update_policy_check_and_install')
    default:
      return t.value('update_policy_off')
  }
})

// `?.` guards a payload older than this setting (or a test fixture that
// predates it): `/api/settings` is deserialized straight from JSON, so
// nothing here enforces at runtime what the type says is never absent.
const updateCadenceLabel = computed(() =>
  props.settings.update_cadence?.kind === 'weekly'
    ? t.value('update_cadence_weekly')
    : t.value('update_cadence_daily'),
)

/**
 * The day of a weekly cadence, read and written through the same computed —
 * `Select` binds to it directly with `v-model`, same idiom as
 * `settings.startup_power` on the view. Reading falls back to Sunday only
 * for the trigger's own label while the cadence is `daily`, where the row is
 * hidden anyway; writing always produces a `weekly` cadence, since this
 * select only exists in the template while one is already selected.
 */
const weeklyDay = computed<Weekday>({
  get: () => (props.settings.update_cadence?.kind === 'weekly' ? props.settings.update_cadence.day : 'sunday'),
  set: (day) => {
    props.settings.update_cadence = { kind: 'weekly', day }
  },
})

const weekdayLabel = computed(() => t.value(`weekday_${weeklyDay.value}`))

/**
 * Switching cadence kind starts a fresh `weekly` at Sunday, or drops to
 * `daily`. `unknown`, not `string`: `Select`'s emitted value is typed for
 * reka-ui's whole `AcceptableValue` union (it also admits `null`), and every
 * value here but `'weekly'` means "daily" regardless of its type.
 */
function onCadenceKindChange(kind: unknown) {
  props.settings.update_cadence = kind === 'weekly' ? { kind: 'weekly', day: 'sunday' } : { kind: 'daily' }
}
</script>

<template>
  <Card data-update-card>
    <CardHeader><CardTitle>{{ t('update_title') }}</CardTitle></CardHeader>
    <CardContent class="space-y-2">
      <UpdateSummary :update="update" />

      <p v-if="errorDetail" data-update-error class="text-sm text-destructive">{{ errorDetail }}</p>
      <!-- A separate line from the error itself: `data-update-error`'s text
           is asserted verbatim by `toBe` in the test suite, so the plain
           statement of this chantier's decision 50-2 (only the first cause of
           a multi-component gesture reaches this payload; every cause is in
           the log) cannot share that element without breaking that test. -->
      <p v-if="errorDetail" data-update-error-note class="text-xs text-muted-foreground">
        {{ t('update_partial_failure_note') }}
      </p>

      <p v-if="installedDetail" data-update-installed class="text-sm text-muted-foreground">
        {{ installedDetail }}
      </p>

      <!-- Without this, the only trace of a 3 a.m. rollback is a version
           number that did not move. -->
      <p v-if="rollbackNote" data-update-rollback class="text-sm text-muted-foreground">
        {{ rollbackNote }}
      </p>

      <p v-if="coreArchiveNoteCount > 0" data-update-core-notes class="text-xs text-muted-foreground">
        {{ t('update_archive_notes', { count: coreArchiveNoteCount }) }}
      </p>

      <p v-if="update.busy" data-update-busy class="text-sm text-muted-foreground">{{ update.busy }}</p>

      <a
        v-if="update.release_url"
        :href="update.release_url"
        target="_blank"
        rel="noopener"
        data-update-release-link
        class="block text-xs text-muted-foreground underline"
      >
        {{ t('update_release_notes') }}
      </a>

      <div class="flex gap-2 pt-2">
        <Button
          variant="secondary"
          data-update-check
          :disabled="!!update.busy"
          @click="emit('check')"
        >
          {{ t('update_check') }}
        </Button>
        <Button
          data-update-install
          :disabled="!!update.busy || actionable.length === 0"
          @click="emit('install')"
        >
          {{ t('update_install') }}
        </Button>
      </div>

      <!-- The separator is what makes this one card legitimate. Above it:
           what is happening now, and two buttons that act at once. Below it:
           what is written in the configuration and waits to be saved. So
           there is exactly **one** save path here, which is what the written
           decision refusing "two save paths behind one title" asked for —
           the decision is honoured, not overturned. Its earlier reading, that
           this card owned no save path, stopped being true the day the
           policy moved in. -->
      <div class="border-t border-border pt-4 space-y-4">
        <!-- Full width, so it reads as its own subject rather than joining
             the row of fields below it as an odd fifth entry — and its label
             names its scope, because it governs every check, the one this
             section schedules and the one "Check" above the separator fires
             by hand. The written decision this card refused elsewhere ("two
             save paths behind one title") still holds: everything below the
             separator, this switch included, waits for the Save button;
             only "Check" and "Install" above the separator act at once. -->
        <label class="flex items-start gap-3 text-sm">
          <Switch
            data-update-prereleases
            :model-value="settings.update_prereleases"
            @update:model-value="(v: boolean) => (settings.update_prereleases = v)"
          />
          <span class="grid gap-1">
            {{ t('update_prereleases_label') }}
            <span class="text-xs text-muted-foreground">{{ t('update_prereleases_help') }}</span>
          </span>
        </label>

        <div class="flex flex-wrap items-end gap-4">
          <label class="grid gap-1 text-sm">
            {{ t('update_policy_label') }}
            <Select v-model="settings.update_policy">
              <SelectTrigger class="min-w-40" data-update-policy :aria-label="t('update_policy_label')">
                <SelectValue>{{ updatePolicyLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="off">{{ t('update_policy_off') }}</SelectItem>
                <SelectItem value="check">{{ t('update_policy_check') }}</SelectItem>
                <SelectItem value="check_and_install">{{ t('update_policy_check_and_install') }}</SelectItem>
              </SelectContent>
            </Select>
          </label>
          <label class="grid gap-1 text-sm">
            {{ t('update_hour_label') }}
            <Input type="number" min="0" max="23" class="w-20" data-update-hour
              v-model="settings.update_hour" />
          </label>
          <label class="grid gap-1 text-sm">
            {{ t('update_cadence_label') }}
            <Select :model-value="settings.update_cadence?.kind ?? 'daily'" @update:model-value="onCadenceKindChange">
              <SelectTrigger class="min-w-32" data-update-cadence :aria-label="t('update_cadence_label')">
                <SelectValue>{{ updateCadenceLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="daily">{{ t('update_cadence_daily') }}</SelectItem>
                <SelectItem value="weekly">{{ t('update_cadence_weekly') }}</SelectItem>
              </SelectContent>
            </Select>
          </label>
          <label v-if="settings.update_cadence?.kind === 'weekly'" class="grid gap-1 text-sm">
            {{ t('update_cadence_day_label') }}
            <Select v-model="weeklyDay">
              <SelectTrigger class="min-w-32" data-update-cadence-day :aria-label="t('update_cadence_day_label')">
                <SelectValue>{{ weekdayLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem v-for="d in WEEKDAYS" :key="d" :value="d">{{ t(`weekday_${d}`) }}</SelectItem>
              </SelectContent>
            </Select>
          </label>
        </div>

        <Button data-update-save @click="emit('save')">{{ t('save') }}</Button>
      </div>
    </CardContent>
  </Card>
</template>
