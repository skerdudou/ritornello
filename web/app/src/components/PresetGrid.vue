<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ChevronLeftIcon, ChevronRightIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'
import { unavailable } from '../views/remoteCommands'

const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null; nameOf: (n: number) => string | null }>()
const emit = defineEmits<{ choose: [n: number] }>()

const page = ref(0)

// Count declared by the source (null = the source says nothing about it: grid
// 1-10, so the remote is never disarmed).
const count = computed(() => props.state?.preset_count ?? null)

// Numbers of the current page, only those that exist. Page k: 10k+1 to
// 10k+10 — so 1-10, 11-20, 21-30. **Same bounds as the core's `+10`**, and
// this is not a coincidence to maintain but a constraint: the web page and the
// physical key must designate the same groups, otherwise "page 2" does not
// mean the same thing depending on whether you look at the screen or the
// remote. Core side, key 0 is worth ten (see `Command::Select`), which is
// exactly what makes ten numbers fit in one decade.
//
// Do not "simplify" into windows of 6 or 12 for layout reasons: the numeric
// keypad only has ten digits.
const presets = computed(() => {
  const c = count.value
  if (c === null) return Array.from({ length: 10 }, (_, i) => i + 1)
  const start = page.value * 10 + 1
  const end = Math.min(page.value * 10 + 10, c)
  return start > end ? [] : Array.from({ length: end - start + 1 }, (_, i) => start + i)
})

const paginationVisible = computed(() => (count.value ?? 0) > 10)

// Last non-empty page. A page k covers 10k+1..10k+10, so the last one that
// holds something is `floor((count - 1) / 10)` — same bound as the wrap-around
// of the core's `+10`, written there as `((count - 1) / 10) * 10`.
const lastPage = computed(() => {
  const c = count.value ?? 0
  return c > 0 ? Math.floor((c - 1) / 10) : 0
})

const window = computed(() => {
  const p = presets.value
  return p.length ? `${p[0]}–${p[p.length - 1]}` : ''
})

function previousPage() {
  if (page.value > 0) page.value -= 1
}
function nextPage() {
  if (page.value < lastPage.value) page.value += 1
}

const activePreset = computed(() => props.state?.preset ?? null)

// The page that holds preset `n` (1-based). `n - 1` because a page covers
// 10k+1..10k+10: 10 belongs to page 0, 11 to page 1.
function pageOf(n: number) {
  return Math.floor((n - 1) / 10)
}

// The page follows what is playing (infrared remote, +10, next track); absent
// a declared preset, a change of count brings back to the first page. A single
// watcher for both fields: they arrive in the same frame. (Moved as is from
// HomeView, with `immediate` added: here `state` arrives as a prop already
// populated at mount time — in HomeView it started at `null` and got populated
// later, which triggered the watch without needing an immediate call.)
watch([count, activePreset], (_, [previousCount]) => {
  if (activePreset.value !== null) {
    page.value = Math.min(pageOf(activePreset.value), lastPage.value)
    return
  }
  if (count.value !== previousCount) page.value = 0
}, { immediate: true })

const dimmed = computed(() => unavailable('Select', props.state))
</script>

<template>
  <div class="space-y-3" data-preset-grid>
    <div v-if="count !== null" class="flex items-center gap-2">
      <!-- The "Presets" label is already the card title (HomeView): here,
           only the count. -->
      <p data-preset-count class="text-xs text-muted-foreground">{{ count }}</p>
      <span class="flex-1" />
      <template v-if="paginationVisible">
        <Button data-preset-prev variant="outline" size="icon-sm" :disabled="page === 0" :aria-label="t('presets_prev_page')" @click="previousPage">
          <ChevronLeftIcon class="size-4" />
        </Button>
        <span class="text-xs tabular-nums text-muted-foreground" data-preset-window>{{ window }}</span>
        <Button data-preset-next variant="outline" size="icon-sm" :disabled="page === lastPage" :aria-label="t('presets_next_page')" @click="nextPage">
          <ChevronRightIcon class="size-4" />
        </Button>
      </template>
    </div>
    <!-- One tile = number + name. Two columns: enough for a station name,
         and the same grid on the phone and in the half-width of the PC. -->
    <div class="grid grid-cols-2 gap-2">
      <Button
        v-for="n in presets"
        :key="n"
        :data-preset-button="n"
        :data-preset-active="state?.preset === n ? 'true' : undefined"
        :aria-current="state?.preset === n ? 'true' : undefined"
        :variant="state?.preset === n ? 'default' : 'outline'"
        class="h-14 justify-start gap-3 px-3 md:h-12"
        :disabled="dimmed"
        @click="emit('choose', n)"
      >
        <span class="w-6 text-left text-base font-bold" :class="state?.preset === n ? '' : 'text-muted-foreground'">{{ n }}</span>
        <span v-if="nameOf(n)" class="truncate font-medium" data-preset-name>{{ nameOf(n) }}</span>
        <span v-if="state?.preset === n" class="ml-auto size-2 shrink-0 rounded-full bg-current" aria-hidden="true" />
      </Button>
    </div>
  </div>
</template>
