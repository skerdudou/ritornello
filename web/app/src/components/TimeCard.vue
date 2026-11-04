<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Select, SelectContent, SelectItem,
  SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onUnmounted, ref, watch } from 'vue'
import {
  browserZone, formatHourMinute, formatOffset, sameZone, zoneChoices,
} from '../composables/timezone'
import { useCatalog } from '../composables/useCatalog'
import type { DateFormat, SystemPayload } from '../types'

/**
 * The device's clock and zone, and the one gesture that changes the zone.
 *
 * It matters because the automatic update fires at an hour counted on the
 * device's clock, and a Pi image ships on UTC or on its author's zone with
 * nothing else on the device saying so.
 *
 * Handed the page's polled `/api/system` payload rather than polling its own:
 * the metrics polling holds an "in flight" lock and computes a CPU delta
 * between two responses (see `SystemView.vue`), so a second reader of the same
 * route rides on it instead of beside it. A zone changed from here therefore
 * shows at the next poll, a few seconds later, not at once.
 */
const props = defineProps<{
  state: SystemPayload | null
  clock: { date_format: DateFormat; clock_24h: boolean }
}>()
const { t } = useCatalog()

/** Em dash for a value the device does not give, as elsewhere on the page. */
const NOTHING = '—'

/**
 * The browser instant at which `device_unix_s` was read, and the browser's
 * clock now: the time shown advances between two polls by what the browser
 * measured, so the minute turns when it turns rather than at the next poll.
 * The device's own clock stays the reference — each poll resets it.
 *
 * One timer per **minute**, aimed at the device's next minute boundary, and
 * not a one-second interval: the card shows `HH:MM`, so fifty-nine renders
 * out of sixty would change nothing — and a one-second interval made the
 * System page's tests, which advance simulated time by many minutes, eighteen
 * times slower (measured: 9 s became 169 s).
 */
const receivedAt = ref(Date.now())
const now = ref(Date.now())
let ticker: ReturnType<typeof setTimeout> | null = null
let alive = true

/** The device's wall clock at browser instant `at`, in milliseconds counted
 *  as if its local time were UTC, so plain arithmetic reads hours and
 *  minutes off it; `null` when the payload lacks the clock or the offset. */
function deviceLocalMs(at: number): number | null {
  const s = props.state
  if (!s || typeof s.device_unix_s !== 'number' || typeof s.utc_offset_seconds !== 'number') return null
  return (s.device_unix_s + s.utc_offset_seconds) * 1000 + Math.max(0, at - receivedAt.value)
}

const MINUTE_MS = 60_000

function schedule(): void {
  if (ticker !== null) clearTimeout(ticker)
  ticker = null
  if (!alive) return
  const local = deviceLocalMs(Date.now())
  // No device time, nothing to advance: the next payload reschedules.
  if (local === null) return
  const intoMinute = ((local % MINUTE_MS) + MINUTE_MS) % MINUTE_MS
  ticker = setTimeout(() => {
    now.value = Date.now()
    schedule()
  }, MINUTE_MS - intoMinute)
}

watch(
  () => props.state?.device_unix_s,
  () => {
    receivedAt.value = Date.now()
    now.value = receivedAt.value
    schedule()
  },
  { immediate: true },
)
onUnmounted(() => {
  alive = false
  if (ticker !== null) clearTimeout(ticker)
})

/** `HH:MM` on the device's wall clock, in the configured 12/24 h format. */
const deviceTime = computed(() => {
  const local = deviceLocalMs(now.value)
  if (local === null) return NOTHING
  const day = 24 * 60
  const minuteOfDay = ((Math.floor(local / MINUTE_MS) % day) + day) % day
  return formatHourMinute(minuteOfDay / 60, props.clock.clock_24h)
})

const zoneName = computed(() => props.state?.timezone ?? t.value('system_time_zone_unknown'))
const zoneOffset = computed(() =>
  typeof props.state?.utc_offset_seconds === 'number' ? formatOffset(props.state.utc_offset_seconds) : '',
)

const browser = browserZone()
/** The shortcut only when it would change something: a device already on
 *  the browser's zone, under any of UTC's names, has nothing to fix. */
const offerBrowser = computed(() => !!props.state && !sameZone(props.state.timezone ?? null, browser))

/**
 * Why the controls are greyed out, one sentence per cause — the same split
 * as the Power card's: timedated not answering calls for a running service,
 * a refusal calls for the installer, where the operator declined the grant.
 */
const unavailable = computed(() => {
  const s = props.state
  if (!s) return null
  if (!s.timedated_reachable) return t.value('system_time_no_timedated')
  if (!s.timezone_settable) return t.value('system_time_not_allowed')
  return null
})

const busy = ref(false)
const error = ref<string | null>(null)
const disabled = computed(() => busy.value || !props.state || unavailable.value !== null)

/**
 * The zone picked in the list. It follows the device's zone until the user
 * picks one, so a poll does not snatch the choice from under them.
 */
const selected = ref('')
const picked = ref(false)
watch(
  () => props.state?.timezone,
  (zone) => {
    if (!picked.value) selected.value = zone ?? ''
  },
  { immediate: true },
)
function pick(value: unknown) {
  selected.value = typeof value === 'string' ? value : ''
  picked.value = true
}

/**
 * The browser's list, plus the device's zone and the browser's own, which a
 * browser without `Intl.supportedValuesOf` would otherwise not offer.
 *
 * Built only while the list is open, and empty otherwise: this card
 * re-renders on every poll of the System page (its payload is a new object
 * each time), and four hundred `SelectItem`s patched every few seconds for a
 * list nobody has open is what made the page's tests time out — measured,
 * one of them went from 3.5 s to past its 20 s ceiling. The trigger names
 * the selection itself (`SelectValue` has content), so a closed list needs
 * none of its items.
 */
const listOpen = ref(false)
const choices = computed(() => {
  if (!listOpen.value) return []
  const all = new Set(zoneChoices())
  if (props.state?.timezone) all.add(props.state.timezone)
  all.add(browser)
  return [...all].sort()
})

async function apply(zone: string) {
  busy.value = true
  error.value = null
  const err = await api.post('/api/system/timezone', { timezone: zone })
  busy.value = false
  if (err) {
    // In the card and not only in a toast: the cause (a name timedated does
    // not know, busctl's own words) is what the operator acts on, and a
    // toast is gone before it is read.
    error.value = err
    return
  }
  picked.value = false
  toast.success(t.value('system_time_zone_changed'))
}
</script>

<template>
  <Card data-time-card>
    <CardHeader><CardTitle>{{ t('system_time') }}</CardTitle></CardHeader>
    <CardContent class="space-y-3 text-sm">
      <div class="grid gap-2 sm:grid-cols-2">
        <div>
          {{ t('system_time_device') }} :
          <span data-time-device class="tabular-nums">{{ deviceTime }}</span>
        </div>
        <div data-time-zone>
          {{ t('system_time_zone') }} : {{ zoneName }}
          <span v-if="zoneOffset" class="text-muted-foreground">({{ zoneOffset }})</span>
        </div>
      </div>

      <p v-if="unavailable" data-time-unavailable class="text-muted-foreground">{{ unavailable }}</p>

      <!-- `flex-wrap` and a bounded trigger: a zone name is as long as
           `America/Argentina/ComodRivadavia`, and the row must not push the
           page sideways on a phone. -->
      <div class="flex flex-wrap items-center gap-2">
        <Select v-model:open="listOpen" :model-value="selected" :disabled="disabled" @update:model-value="pick">
          <SelectTrigger data-time-zone-select class="w-64 max-w-full" :aria-label="t('system_time_zone')">
            <SelectValue>{{ selected || NOTHING }}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem v-for="z in choices" :key="z" :value="z">{{ z }}</SelectItem>
          </SelectContent>
        </Select>
        <Button
          data-time-apply
          :disabled="disabled || !selected || selected === state?.timezone"
          @click="apply(selected)"
        >
          {{ t('system_time_apply') }}
        </Button>
        <Button
          v-if="offerBrowser"
          variant="outline"
          data-time-use-browser
          :disabled="disabled"
          @click="apply(browser)"
        >
          {{ t('system_time_use_browser', { zone: browser }) }}
        </Button>
      </div>

      <p v-if="error" data-time-error class="text-destructive">{{ error }}</p>
    </CardContent>
  </Card>
</template>
