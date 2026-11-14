<script setup lang="ts">
import {
  Button, Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger,
} from '@ritornello/ui'
import { computed, onUnmounted, ref, watch } from 'vue'
import SleepTimerIcon from './icons/SleepTimerIcon.vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command, PlayerPayload, SleepMode } from '../types'
import { unavailable } from '../views/remoteCommands'
import {
  formatRemaining, remainingSeconds, SLEEP_MODES, SLEEP_TIMER_EXTEND_MIN, SLEEP_TIMER_MINUTES,
} from './sleepTimer'

/**
 * The sleep timer key of the Player card's header, beside standby: both act
 * on the whole device rather than on what plays.
 *
 * At rest it is a bare moon. While a timer runs it reads the time left,
 * counted down **here**, once a second, from the deadline the core pushed
 * (`sleep_timer.ends_at_unix_s`): the core publishes the deadline on each
 * change and never a frame per second for the timer alone, and this key
 * asks for nothing either. Once the deadline has passed the key reads what
 * the device is doing instead — waiting for the track to end (the deadline
 * is then only the 5-minute cap, not worth counting down) or fading out.
 *
 * A click opens the menu: the end mode, then the durations, each of which
 * starts a timer at once (replacing a running one); while a timer runs,
 * "+15 min" and "Cancel" come first. A dialog rather than a dropdown: the
 * kit has no dropdown menu, and its dialog brings the focus trap, Escape
 * and the phone-sized layout for free.
 *
 * Disabled in standby: the core ignores `SetSleepTimer` there (there is
 * nothing to put to sleep), so the key must not pretend otherwise — the rule
 * of `unavailable`.
 */
const props = defineProps<{
  state: PlayerPayload | null
  /** The end mode the core remembers (`settings.sleep_timer_mode`). */
  mode: SleepMode
}>()
const emit = defineEmits<{ command: [cmd: Command] }>()
const { t } = useCatalog()

const open = ref(false)

// The mode the menu opens on: the remembered one, then the last one this
// page started a timer with (the core remembers it too, but the settings
// are read once, on mount). A mode picked and left without starting a timer
// is forgotten at the next opening, as the core forgets it.
const remembered = ref<SleepMode>(props.mode)
watch(() => props.mode, (m) => (remembered.value = m))
const chosen = ref<SleepMode>(props.mode)
watch(open, (isOpen) => {
  if (isOpen) chosen.value = remembered.value
})

const timer = computed(() => props.state?.sleep_timer ?? null)
const disabled = computed(() => unavailable('SetSleepTimer', props.state))

// The local clock of the countdown: ticks only while a timer is armed.
const now = ref(Date.now())
let tick: ReturnType<typeof setInterval> | null = null
function stopTick() {
  if (tick !== null) clearInterval(tick)
  tick = null
}
watch(
  () => timer.value?.phase === 'armed',
  (armed) => {
    stopTick()
    now.value = Date.now()
    if (armed) tick = setInterval(() => (now.value = Date.now()), 1000)
  },
  { immediate: true },
)
// A new deadline (+15 min, another timer) is read at once, not at the next tick.
watch(() => timer.value?.ends_at_unix_s, () => (now.value = Date.now()))
onUnmounted(stopTick)

const keyText = computed(() => {
  const st = timer.value
  if (!st) return ''
  if (st.phase === 'fading') return t.value('sleep_timer_fading')
  if (st.phase === 'waiting_end_of_track') return t.value('sleep_timer_waiting')
  return formatRemaining(remainingSeconds(st.ends_at_unix_s, now.value))
})
const keyLabel = computed(() => (keyText.value ? `${t.value('sleep_timer')} ${keyText.value}` : t.value('sleep_timer')))

function modeLabel(m: SleepMode): string {
  // Literal keys, one per mode: `i18nKeysUsed.test.ts` only sees literals.
  switch (m) {
    case 'immediate': return t.value('sleep_timer_mode_immediate')
    case 'fade': return t.value('sleep_timer_mode_fade')
    case 'end_of_track': return t.value('sleep_timer_mode_end_of_track')
  }
}

function fire(cmd: Command) {
  open.value = false
  emit('command', cmd)
}
function start(minutes: number) {
  remembered.value = chosen.value
  fire({ cmd: 'SetSleepTimer', arg: { minutes, mode: chosen.value } })
}
</script>

<template>
  <Dialog v-model:open="open">
    <DialogTrigger as-child>
      <Button
        variant="outline"
        :size="keyText ? 'sm' : 'icon-sm'"
        data-sleep-timer-key
        :data-sleep-phase="timer?.phase"
        :disabled="disabled"
        :aria-label="keyLabel"
        :title="t('sleep_timer')"
      >
        <SleepTimerIcon class="size-4" />
        <span v-if="keyText" class="tabular-nums">{{ keyText }}</span>
      </Button>
    </DialogTrigger>
    <DialogContent :aria-describedby="undefined" class="sm:max-w-sm" data-sleep-timer-menu>
      <DialogHeader>
        <DialogTitle>{{ t('sleep_timer') }}</DialogTitle>
      </DialogHeader>
      <div v-if="timer" class="flex flex-wrap items-center gap-2">
        <span class="mr-auto text-sm text-muted-foreground tabular-nums">
          {{ timer.phase === 'armed' ? t('sleep_timer_remaining', { time: keyText }) : keyText }}
        </span>
        <Button
          variant="outline"
          size="sm"
          data-sleep-extend
          @click="fire({ cmd: 'ExtendSleepTimer', arg: SLEEP_TIMER_EXTEND_MIN })"
        >
          {{ t('sleep_timer_extend', { n: SLEEP_TIMER_EXTEND_MIN }) }}
        </Button>
        <Button variant="outline" size="sm" data-sleep-cancel @click="fire({ cmd: 'CancelSleepTimer' })">
          {{ t('sleep_timer_cancel') }}
        </Button>
      </div>
      <div class="grid gap-2">
        <span id="sleep-timer-when" class="text-sm font-medium">{{ t('sleep_timer_when') }}</span>
        <div role="radiogroup" aria-labelledby="sleep-timer-when" class="flex flex-wrap gap-2">
          <Button
            v-for="m in SLEEP_MODES"
            :key="m"
            role="radio"
            :aria-checked="chosen === m ? 'true' : 'false'"
            :variant="chosen === m ? 'default' : 'outline'"
            size="sm"
            :data-sleep-mode="m"
            @click="chosen = m"
          >
            {{ modeLabel(m) }}
          </Button>
        </div>
      </div>
      <div class="grid gap-2">
        <span id="sleep-timer-start" class="text-sm font-medium">{{ t('sleep_timer_start') }}</span>
        <div role="group" aria-labelledby="sleep-timer-start" class="grid grid-cols-5 gap-2">
          <Button
            v-for="n in SLEEP_TIMER_MINUTES"
            :key="n"
            variant="outline"
            size="sm"
            :data-sleep-minutes="n"
            @click="start(n)"
          >
            {{ t('sleep_timer_minutes', { n }) }}
          </Button>
        </div>
      </div>
    </DialogContent>
  </Dialog>
</template>
