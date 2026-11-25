<script setup lang="ts" generic="T">
import { computed, provide, shallowRef, watch } from "vue"
import { useDraggable, type SortableEvent } from "vue-draggable-plus"
import { moveAmong } from "@/order"
import { REORDER_DISABLED } from "./context"

/**
 * A list whose rows are reordered by dragging their `ReorderHandle`, with a
 * finger or a mouse alike.
 *
 * SortableJS does the dragging; **Vue keeps the DOM**. Sortable moves the
 * grabbed element while the finger holds it, and the moment it lets go the
 * move is undone and the order handed to the parent as two positions. The
 * list on screen is then the dropped order until the parent's answer settles,
 * and the parent's own list after that: a refused or failed move falls back
 * by itself, with nothing for the parent to restore. No second drag starts
 * while a drop awaits its answer: its positions would count the dropped order
 * the parent does not hold yet, and the first answer would wipe its display.
 *
 * The doc lives here and not above the template on purpose: an HTML comment
 * before the root element would give the component two root nodes, and the
 * fallthrough attributes would land nowhere.
 */
const props = withDefaults(defineProps<{
  items: readonly T[]
  itemKey: (item: T, index: number) => string | number
  /** The container element: `tbody` for a table. */
  tag?: string
  disabled?: boolean
  /** Which items can be grabbed; the others keep their place. */
  reorderable?: (item: T) => boolean
  /**
   * A prop and not an emit, so the component can await what it returns:
   * the dropped order stays on screen until that settles. `from` and `to` are
   * positions **among reorderable items**, as Sortable counts them, in the
   * list as it was when the drag began. `item` is the row the finger held,
   * taken from that same list: when `items` changed during the drag (a poll),
   * `items` no longer agrees with `from`, and `item` is what names the row.
   * No second drop until the returned promise settles: the list is disabled
   * meanwhile. A throw puts the list back and is logged; a rejection only
   * puts it back, the parent being the one to report a refusal.
   */
  onMove?: (from: number, to: number, item: T) => unknown
}>(), {
  tag: "div",
  disabled: false,
  reorderable: () => true,
})

defineSlots<{
  default(props: { item: T; index: number; itemAttrs: Record<string, string> }): unknown
}>()

// The marker Sortable's `draggable` selector looks for. It goes on the item's
// root row only: a detail row under it follows the undo, never the finger.
const MARKED: Record<string, string> = Object.freeze({ "data-reorder-item": "" }) as Record<string, string>
const UNMARKED: Record<string, string> = Object.freeze({}) as Record<string, string>

function isReorderable(item: T): boolean {
  return props.reorderable(item)
}

function attrsFor(item: T): Record<string, string> {
  return isReorderable(item) ? MARKED : UNMARKED
}

const snapshot = shallowRef<readonly T[] | null>(null) // frozen while a finger holds a row
const pending = shallowRef<readonly T[] | null>(null) // dropped order, until the parent answers
const shown = computed(() => pending.value ?? snapshot.value ?? props.items)
// Disabled by the parent, or waiting for the parent's answer to a drop.
const locked = computed(() => props.disabled || pending.value !== null)
// The node the grabbed element was in front of when the drag began: where it
// goes back to. It may be one of Vue's empty text anchors, which is fine.
let anchor: Node | null = null

// The row is grabbed (after the hold, for a finger): say so to the hand,
// where the device can vibrate. The highlight is `reorder-chosen`'s, below.
function onChoose() {
  navigator.vibrate?.(15)
}

function onStart(e: SortableEvent) {
  // A poll that lands mid-drag must not re-render the rows under the finger:
  // Vue would patch a DOM that Sortable has rearranged.
  // The parent's list is what is on screen: no drag starts while a drop is
  // pending (see `locked`).
  snapshot.value = [...props.items]
  anchor = e.item.nextSibling
}

function onEnd(e: SortableEvent) {
  const base = snapshot.value
  if (!base) return
  // Undo Sortable's move: Vue owns this DOM and must find it as it rendered it.
  if (e.item.nextSibling !== anchor) e.from.insertBefore(e.item, anchor)
  snapshot.value = null
  const { oldDraggableIndex: from, newDraggableIndex: to } = e
  if (from == null || to == null || from === to || !props.onMove) return
  // The grabbed row, read from the list the positions count: the snapshot.
  const item = base.filter(isReorderable)[from]
  if (item === undefined) return
  pending.value = moveAmong(base, isReorderable, from, to)
  let answer: unknown
  try {
    answer = props.onMove(from, to, item)
  } catch (e) {
    // A defect in the caller, which nothing else would ever show.
    pending.value = null
    console.error(e)
    return
  }
  // Whatever the answer -- accepted, refused, failed -- the parent's list is
  // the truth once it settles. A rejection is the parent's to report; here it
  // only ends the wait, and must not surface as an unhandled rejection.
  void Promise.resolve(answer).catch(() => {}).finally(() => {
    pending.value = null
  })
}

const root = shallowRef<HTMLElement | null>(null)

// Two arguments, no list: given a list, the library would splice it and move
// DOM nodes itself, behind Vue's back. Our `onStart`/`onEnd` are the only
// handlers.
const draggable = useDraggable<T>(root, {
  // Only the handle starts a drag: the rest of the row stays free to scroll
  // the page with a finger, and to select or click.
  handle: "[data-reorder-handle]",
  draggable: "[data-reorder-item]",
  direction: "vertical",
  // Sortable's own pointer-driven drag instead of native HTML5 drag-and-drop,
  // which touch screens do not fire: one code path for finger and mouse.
  forceFallback: true,
  // The floating copy stays inside the list rather than under `<body>`, where
  // a `<tr>` out of its table has nothing to render into.
  fallbackOnBody: false,
  // A few pixels before a press becomes a drag, so a tap on the handle stays
  // a tap.
  fallbackTolerance: 3,
  // A finger must hold the handle still for 0.4 s before the row is grabbed;
  // moving more than 5 px meanwhile cancels the grab. The handle refuses to
  // scroll (`touch-none`), so without the hold a thumb landing on it to
  // scroll the page moved a row instead. A mouse grabs at once.
  delay: 400,
  delayOnTouchOnly: true,
  touchStartThreshold: 5,
  ghostClass: "reorder-ghost",
  chosenClass: "reorder-chosen",
  fallbackClass: "reorder-fallback",
  // Long lists on a phone: holding a row near the edge scrolls, starting
  // 120 px from it, and any scrollable ancestor (the page itself) as well as
  // the list.
  scroll: true,
  bubbleScroll: true,
  scrollSensitivity: 120,
  scrollSpeed: 20,
  disabled: locked.value,
  onChoose,
  onStart,
  onEnd,
})

watch(locked, (d) => draggable.option("disabled", d))

// The handles show the same lock, a pending drop included.
provide(REORDER_DISABLED, locked)

// Sortable names its states with single classes it sets itself; arbitrary
// variants on the container style them. Here and not in `theme.css`: the
// shell's Tailwind pass scans the kit's sources, and leaving `theme.css`
// alone leaves every plugin's `ui.css` unchanged.
const ROOT_CLASS = [
  // The grabbed row, from the end of the hold: the finger may move now.
  "[&_.reorder-chosen]:bg-accent",
  // The placeholder, where the row will land: the feedback the finger sees.
  "[&_.reorder-ghost]:bg-accent [&_.reorder-ghost]:opacity-60",
  "[&_.reorder-ghost]:outline-2 [&_.reorder-ghost]:outline-dashed [&_.reorder-ghost]:outline-primary",
  // The floating copy is hidden: a `position: fixed` `<tr>` loses its column
  // widths and would float as a squashed strip. The placeholder suffices.
  "[&_.reorder-fallback]:opacity-0",
].join(" ")
</script>

<template>
  <component :is="tag" ref="root" :class="ROOT_CLASS">
    <template v-for="(item, i) in shown" :key="itemKey(item, i)">
      <slot :item="item" :index="i" :item-attrs="attrsFor(item)" />
    </template>
  </component>
</template>
