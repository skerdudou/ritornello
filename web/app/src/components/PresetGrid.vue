<script setup lang="ts">
import { Button, Card, CardAction, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { ChevronLeftIcon, ChevronRightIcon, ListBulletIcon, ViewGridIcon } from '@radix-icons/vue'
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import {
  classifySwipe,
  loadPresetLayout,
  revealScrollTop,
  savePresetLayout,
  settleDirection,
  SWIPE_RATIO,
  type PresetLayout,
} from './presetLayout'
import type { PlayerPayload } from '../types'
import { unavailable } from '../views/remoteCommands'

const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null; nameOf: (n: number) => string | null; listedCount: number }>()
const emit = defineEmits<{ choose: [n: number] }>()

const page = ref(0)

// Count declared by the source (null = the source says nothing about it: grid
// 1-10, so the remote is never disarmed).
// In standby the core forgets the count the source announced: the catalog
// (`/api/presets`, kept in standby) says which presets exist, 0 for a source
// that lists none (a cd) — an empty grid rather than ten dead tiles.
const count = computed(() => (props.state?.standby ? props.listedCount : props.state?.preset_count ?? null))

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
// A page outside the range (before the first, after the last) is empty: during
// a drag it is the neighbour drawn beside the current one.
function pageNumbers(k: number): number[] {
  const c = count.value
  if (c === null) return k === 0 ? Array.from({ length: 10 }, (_, i) => i + 1) : []
  const start = k * 10 + 1
  const end = Math.min(k * 10 + 10, c)
  return k < 0 || start > end ? [] : Array.from({ length: end - start + 1 }, (_, i) => start + i)
}
const presets = computed(() => pageNumbers(page.value))

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
  return p.length ? t.value('presets_window', { first: p[0]!, last: p[p.length - 1]!, total: count.value ?? 0 }) : ''
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

// Pages of ten (default) or one scrolling list of every preset, remembered per
// browser (see presetLayout.ts). The toggle is always offered, whatever the
// count: the list also suits a short list.
const layout = ref<PresetLayout>(loadPresetLayout())
const listMode = computed(() => layout.value === 'list')
function toggleLayout() {
  layout.value = layout.value === 'list' ? 'pages' : 'list'
  savePresetLayout(layout.value)
}

// Every preset, for the list: 1..10 without a declared count (same fallback as
// the grid), 1..count otherwise.
const allPresets = computed(() => {
  const c = count.value ?? 10
  return Array.from({ length: c }, (_, i) => i + 1)
})

// In the list, keep the active preset visible by scrolling the frame itself,
// never `scrollIntoView`: that would also scroll the page, which jumps under
// the thumb on a phone. (No `window.*` here: `window` is the computed above.)
const listFrame = ref<HTMLElement | null>(null)
function revealActive() {
  const frame = listFrame.value
  const n = activePreset.value
  if (!listMode.value || n === null || !frame) return
  const row = frame.querySelector<HTMLElement>(`[data-preset-button="${n}"]`)
  if (!row) return
  frame.scrollTop = revealScrollTop(frame.scrollTop, frame.clientHeight, row.offsetTop, row.offsetHeight, frame.scrollHeight)
}
watch([listMode, activePreset], revealActive, { flush: 'post' })
// Not `immediate`: that runs during setup, before the frame exists.
onMounted(revealActive)

// A horizontal swipe on the card turns the page, pages mode only (see
// classifySwipe). Touch and pen only: a mouse drag is a text selection, not a
// gesture. `touch-action: pan-y pinch-zoom` on the card keeps the browser's
// vertical page scroll and the two-finger zoom the page allows; when either
// takes over (a second finger too) it fires `pointercancel`, which aborts the
// swipe. Hand-written Pointer Events: no second library for ten lines.
let start: { id: number; x: number; y: number } | null = null
// The click that ends a swipe arrives right after its pointerup; many mobile
// browsers send none at all after a drag, so the swallow expires rather than
// waiting for "the next click", which would then be an unrelated one.
const SWALLOW_WINDOW_MS = 500
let swallowUntil = 0

// The page follows the finger. Once a horizontal move passes DRAG_START_PX
// (with the swipe's own ratio), the track shows the neighbouring pages beside
// the current one and translates with the finger. On release it settles over
// SETTLE_MS: onto the neighbour when the drag commits, back to the current page
// otherwise; the page changes once the settle is done.
const DRAG_START_PX = 10
const SETTLE_MS = 200
type Settle = 'next' | 'previous' | 'back'
// Pixels the finger has moved, null when no drag is shown.
const dragOffset = ref<number | null>(null)
// Set from the release until the settle ends; null while the finger is down.
const settling = ref<Settle | null>(null)
let settleTimer: ReturnType<typeof setTimeout> | undefined
onBeforeUnmount(() => clearTimeout(settleTimer))

// Three pages wide while dragging, the current page in the middle: -33.3333%
// of that track is one page, so the middle page sits in view and a neighbour
// is revealed on either side.
const trackStyle = computed(() => {
  if (dragOffset.value === null) return undefined
  const base = settling.value === 'next' ? '-66.6667%' : settling.value === 'previous' ? '0%' : '-33.3333%'
  const px = settling.value === null ? dragOffset.value : 0
  return { width: '300%', transform: `translateX(calc(${base} + ${px}px))` }
})
const panels = computed(() => {
  if (listMode.value) return [allPresets.value]
  if (dragOffset.value === null) return [presets.value]
  return [pageNumbers(page.value - 1), pageNumbers(page.value), pageNumbers(page.value + 1)]
})

function onPointerDown(e: PointerEvent) {
  // Any new gesture, whatever the pointer, ends the swallow.
  swallowUntil = 0
  if (e.pointerType !== 'touch' && e.pointerType !== 'pen') return
  // A second finger is ignored; a new primary pointer always starts afresh,
  // so a start whose up never reached the card (a pen lifted off it) cannot
  // go stale. Nothing starts while a settle is running.
  if (listMode.value || !e.isPrimary || settling.value !== null) return
  start = { id: e.pointerId, x: e.clientX, y: e.clientY }
}
function onPointerMove(e: PointerEvent) {
  if (!start || start.id !== e.pointerId) return
  const dx = e.clientX - start.x
  const dy = e.clientY - start.y
  if (dragOffset.value === null && (Math.abs(dx) < DRAG_START_PX || Math.abs(dx) < SWIPE_RATIO * Math.abs(dy))) return
  // A direction with no page behind it does not move: the page stays put.
  const blocked = (dx < 0 && page.value >= lastPage.value) || (dx > 0 && page.value <= 0)
  dragOffset.value = blocked ? 0 : dx
}
function onPointerUp(e: PointerEvent) {
  if (!start || start.id !== e.pointerId) return
  const dx = e.clientX - start.x
  const dy = e.clientY - start.y
  start = null
  if (dragOffset.value === null) {
    // No move was seen (a flick the browser reported only at its ends): judged
    // as a plain swipe, and the page turns without a settle.
    const direction = classifySwipe(dx, dy)
    if (!direction) return
    if (direction === 'next') nextPage()
    else previousPage()
    swallowUntil = e.timeStamp + SWALLOW_WINDOW_MS
    return
  }
  release(settleDirection(dx, dy, listFrame.value?.clientWidth ?? 0))
  swallowUntil = e.timeStamp + SWALLOW_WINDOW_MS
}
function onPointerCancel() {
  start = null
  // A drag the browser took over (a vertical scroll, a second finger) springs back.
  if (dragOffset.value !== null && settling.value === null) release(null)
}
// `direction` is what the release decided; a commit past the last or first page
// springs back as well.
function release(direction: 'next' | 'previous' | null) {
  const target: Settle =
    direction === 'next' && page.value < lastPage.value ? 'next'
    : direction === 'previous' && page.value > 0 ? 'previous'
    : 'back'
  settling.value = target
  clearTimeout(settleTimer)
  settleTimer = setTimeout(() => {
    if (target === 'next') nextPage()
    else if (target === 'previous') previousPage()
    dragOffset.value = null
    settling.value = null
  }, SETTLE_MS)
}
// The browser still sends a click when a swipe ends on the element it started
// on: without this, swiping from a tile would also choose that preset and
// change what is playing.
function onClickCapture(e: Event) {
  const swallow = swallowUntil !== 0 && e.timeStamp <= swallowUntil
  swallowUntil = 0
  if (!swallow) return
  e.preventDefault()
  e.stopPropagation()
}

// The page arrows stay usable in standby on purpose: paging is local to the
// page and sends nothing.
const dimmed = computed(() => unavailable('Select', props.state))
</script>

<template>
  <Card
    data-preset-grid
    :data-preset-layout="listMode ? 'list' : 'pages'"
    :style="listMode ? undefined : { touchAction: 'pan-y pinch-zoom' }"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerCancel"
    @click.capture="onClickCapture"
  >
    <CardHeader class="items-center">
      <CardTitle>{{ t('presets_label') }}</CardTitle>
      <!-- The layout toggle is always there; the position and the arrows only
           when there are pages (and not in the list): a lone count read as a
           stray number under the title. Same row as the title, as the
           Player card holds its keys. -->
      <CardAction class="flex items-center gap-2 self-center">
        <template v-if="paginationVisible && !listMode">
          <Button data-preset-prev variant="outline" size="icon-sm" :disabled="page === 0" :aria-label="t('presets_prev_page')" @click="previousPage">
            <ChevronLeftIcon class="size-4" />
          </Button>
          <span class="text-xs tabular-nums text-muted-foreground" data-preset-window>{{ window }}</span>
          <Button data-preset-next variant="outline" size="icon-sm" :disabled="page === lastPage" :aria-label="t('presets_next_page')" @click="nextPage">
            <ChevronRightIcon class="size-4" />
          </Button>
        </template>
        <!-- The icon names the layout the button switches to. -->
        <Button
          data-preset-layout-toggle
          variant="outline"
          size="icon-sm"
          :aria-label="listMode ? t('presets_show_pages') : t('presets_show_list')"
          :title="listMode ? t('presets_show_pages') : t('presets_show_list')"
          @click="toggleLayout"
        >
          <ViewGridIcon v-if="listMode" class="size-4" />
          <ListBulletIcon v-else class="size-4" />
        </Button>
      </CardAction>
    </CardHeader>
    <CardContent>
      <!-- One tile = number + name. Pages: two columns, enough for a station
           name, and the same grid on the phone and in the half-width of the PC.
           List: one column in a scrolling frame, every preset. -->
      <!-- The frame is the scroller in list mode, and the window on the pages
           in pages mode. The list's scrollbar is inset from the card's right
           edge (`mr-2`), so it does not sit flush against it. -->
      <div
        ref="listFrame"
        :data-preset-list="listMode ? '' : undefined"
        :class="listMode
          ? 'relative mr-2 max-h-[min(60vh,32rem)] overflow-y-auto p-1'
          : 'overflow-hidden'"
      >
        <div
          data-preset-track
          :class="[listMode ? '' : 'flex', settling ? 'transition-transform duration-200 ease-out' : '']"
          :style="listMode ? undefined : trackStyle"
        >
          <div
            v-for="(group, i) in panels"
            :key="i"
            :data-preset-panel="listMode ? undefined : i"
            :class="listMode ? 'grid grid-cols-1 gap-2' : ['grid shrink-0 grid-cols-2 gap-2', dragOffset === null ? 'w-full' : 'w-1/3']"
          >
            <Button
              v-for="n in group"
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
      </div>
    </CardContent>
  </Card>
</template>
