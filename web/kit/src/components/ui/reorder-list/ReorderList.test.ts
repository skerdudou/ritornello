import { enableAutoUnmount, flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { h, nextTick, type VNode } from 'vue'
import { useDraggable } from 'vue-draggable-plus'
import { ReorderHandle, ReorderList } from '.'

// What the component hands to Sortable is the whole contract with the
// library: capture it rather than drive a real drag, which jsdom cannot do
// (no layout, no pointer geometry).
const option = vi.fn()
let opts: Record<string, any> = {}
vi.mock('vue-draggable-plus', () => ({
  useDraggable: vi.fn((_el: unknown, o: Record<string, any>) => {
    opts = o
    return { option }
  }),
}))

type SlotProps = { item: string; index: number; itemAttrs: Record<string, string> }

const oneRow = (p: SlotProps) => h('li', { ...p.itemAttrs, 'data-k': p.item }, p.item)

function mountList(
  props: Record<string, unknown> = {},
  slot: (p: SlotProps) => VNode | VNode[] = oneRow,
): VueWrapper<any> {
  return mount(ReorderList as any, {
    props: { items: ['a', 'b', 'c'], itemKey: (x: string) => x, tag: 'ul', ...props },
    slots: { default: slot },
    attachTo: document.body,
  })
}

// Element children only: a keyed `<template v-for>` leaves empty text anchors
// between the rows, which are not rows.
function order(w: VueWrapper<any>): (string | null)[] {
  return Array.from((w.element as HTMLElement).children).map((el) => el.getAttribute('data-k'))
}

function li(w: VueWrapper<any>, key: string): HTMLElement {
  return w.element.querySelector(`[data-k="${key}"]`) as HTMLElement
}

/** Sortable's own move of `key` before `before` (or to the end), between its two events. */
function drop(w: VueWrapper<any>, key: string, before: string | null, oldIndex: number, newIndex: number) {
  const root = w.element as HTMLElement
  const item = li(w, key)
  opts.onStart({ item })
  root.insertBefore(item, before ? li(w, before) : null)
  opts.onEnd({ item, from: root, oldDraggableIndex: oldIndex, newDraggableIndex: newIndex })
}

enableAutoUnmount(afterEach)

beforeEach(() => {
  option.mockClear()
  vi.mocked(useDraggable).mockClear()
  opts = {}
})

describe('ReorderList', () => {
  it('hands Sortable the handle, the item marker and the scroll tuning', () => {
    mountList()
    expect(opts).toMatchObject({
      handle: '[data-reorder-handle]',
      draggable: '[data-reorder-item]',
      direction: 'vertical',
      forceFallback: true,
      fallbackOnBody: false,
      fallbackTolerance: 3,
      ghostClass: 'reorder-ghost',
      chosenClass: 'reorder-chosen',
      fallbackClass: 'reorder-fallback',
      scroll: true,
      bubbleScroll: true,
      scrollSensitivity: 120,
      scrollSpeed: 20,
    })
    // Two arguments, no list: given a list, the library would mutate it
    // (and the DOM) itself, behind Vue's back.
    expect(vi.mocked(useDraggable)).toHaveBeenCalledTimes(1)
    expect(vi.mocked(useDraggable).mock.calls[0]).toHaveLength(2)
  })

  it('follows disabled', async () => {
    const w = mountList({ disabled: true })
    expect(opts.disabled).toBe(true)
    await w.setProps({ disabled: false })
    expect(option).toHaveBeenLastCalledWith('disabled', false)
    await w.setProps({ disabled: true })
    expect(option).toHaveBeenLastCalledWith('disabled', true)
  })

  it("a drop shows the dropped order until the parent answers, then the parent's list", async () => {
    let answer!: () => void
    const onMove = vi.fn(() => new Promise<void>((r) => { answer = r }))
    const w = mountList({ onMove })
    drop(w, 'c', 'a', 2, 0)
    await nextTick()
    expect(onMove).toHaveBeenCalledTimes(1)
    expect(onMove).toHaveBeenCalledWith(2, 0)
    expect(order(w)).toEqual(['c', 'a', 'b'])
    await w.setProps({ items: ['c', 'a', 'b'] })
    answer()
    await flushPromises()
    expect(order(w)).toEqual(['c', 'a', 'b'])
  })

  it("a refused drop falls back to the parent's list", async () => {
    const w = mountList({ onMove: vi.fn(() => Promise.resolve()) })
    drop(w, 'c', 'a', 2, 0)
    await nextTick()
    expect(order(w)).toEqual(['c', 'a', 'b'])
    await flushPromises()
    expect(order(w)).toEqual(['a', 'b', 'c'])

    // A rejection too: the component swallows it once the display is cleared,
    // so no unhandled rejection reaches the test runner.
    const r = mountList({ onMove: vi.fn(() => Promise.reject(new Error('refused'))) })
    drop(r, 'c', 'a', 2, 0)
    await nextTick()
    expect(order(r)).toEqual(['c', 'a', 'b'])
    await flushPromises()
    expect(order(r)).toEqual(['a', 'b', 'c'])
  })

  it('a drop in place calls nothing', async () => {
    const onMove = vi.fn()
    const w = mountList({ onMove })
    drop(w, 'b', 'c', 1, 1)
    const item = li(w, 'a')
    opts.onStart({ item })
    opts.onEnd({ item, from: w.element, oldDraggableIndex: undefined, newDraggableIndex: 1 })
    expect(() =>
      opts.onEnd({ item, from: w.element, oldDraggableIndex: 0, newDraggableIndex: 2 }),
    ).not.toThrow()
    await nextTick()
    expect(onMove).not.toHaveBeenCalled()
    expect(order(w)).toEqual(['a', 'b', 'c'])
  })

  it('a two-element item is put back whole', async () => {
    const w = mountList({ onMove: vi.fn() }, (p) => [
      h('li', { ...p.itemAttrs, 'data-k': p.item }, p.item),
      h('li', { 'data-k': `${p.item}-detail` }, 'detail'),
    ])
    // Sortable moves the one element it grabbed, here after its own detail
    // row; nothing else in the component would put it back.
    drop(w, 'a', 'b', 0, 0)
    await nextTick()
    expect(order(w)).toEqual(['a', 'a-detail', 'b', 'b-detail', 'c', 'c-detail'])
  })

  it('items changing during a drag do not re-render under the finger', async () => {
    const w = mountList({ onMove: vi.fn() })
    const item = li(w, 'a')
    opts.onStart({ item })
    await w.setProps({ items: ['x', 'a', 'b', 'c'] })
    await nextTick()
    expect(order(w)).toEqual(['a', 'b', 'c'])
    opts.onEnd({ item, from: w.element, oldDraggableIndex: 0, newDraggableIndex: 0 })
    await nextTick()
    expect(order(w)).toEqual(['x', 'a', 'b', 'c'])
  })

  it('only reorderable items are marked and counted', async () => {
    const onMove = vi.fn(() => new Promise(() => {}))
    const w = mountList({ onMove, reorderable: (x: string) => x !== 'b' })
    expect(li(w, 'a').hasAttribute('data-reorder-item')).toBe(true)
    expect(li(w, 'b').hasAttribute('data-reorder-item')).toBe(false)
    expect(li(w, 'c').hasAttribute('data-reorder-item')).toBe(true)
    drop(w, 'c', 'a', 1, 0)
    await nextTick()
    expect(onMove).toHaveBeenCalledWith(1, 0)
    expect(order(w)).toEqual(['c', 'b', 'a'])
  })
})

describe('ReorderHandle', () => {
  it('ReorderHandle is a touch-only grab hidden from assistive tech', () => {
    const alone = mount(ReorderHandle, { attrs: { title: 'Drag to reorder' } })
    const span = alone.element as HTMLElement
    expect(span.hasAttribute('data-reorder-handle')).toBe(true)
    expect(span.getAttribute('aria-hidden')).toBe('true')
    expect(span.getAttribute('title')).toBe('Drag to reorder')
    expect(span.classList).toContain('touch-none')
    expect(span.classList).toContain('select-none')
    expect(span.classList).toContain('cursor-grab')

    const w = mountList({ disabled: true }, (p) =>
      h('li', { ...p.itemAttrs, 'data-k': p.item }, [h(ReorderHandle)]),
    )
    const inList = w.find('[data-reorder-handle]').element as HTMLElement
    expect(inList.classList).toContain('cursor-not-allowed')
    expect(inList.classList).not.toContain('cursor-grab')
  })
})
