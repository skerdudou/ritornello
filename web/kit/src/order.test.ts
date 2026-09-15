import { describe, expect, it } from 'vitest'
import { move } from './order'

describe('move', () => {
  it('moves down and up', () => {
    expect(move(['A', 'B', 'C'], 0, 2)).toEqual(['B', 'C', 'A'])
    expect(move(['A', 'B', 'C'], 2, 0)).toEqual(['C', 'A', 'B'])
    expect(move(['A', 'B', 'C'], 1, 2)).toEqual(['A', 'C', 'B'])
  })

  it('does not modify the original list', () => {
    // The list is a Vue `ref`: mutating it in place would bypass
    // reactivity on some code paths and would make component tests
    // dependent on assertion order.
    const original = ['A', 'B', 'C']
    move(original, 0, 2)
    expect(original).toEqual(['A', 'B', 'C'])
  })

  it('leaves the list unchanged on an impossible move', () => {
    // The indices come from the browser's drag-and-drop events: a target
    // can disappear between `dragstart` and `drop`.
    expect(move(['A', 'B'], 0, 0)).toEqual(['A', 'B'])
    expect(move(['A', 'B'], -1, 1)).toEqual(['A', 'B'])
    expect(move(['A', 'B'], 0, 5)).toEqual(['A', 'B'])
    expect(move(['A', 'B'], 9, 0)).toEqual(['A', 'B'])
    expect(move([], 0, 0)).toEqual([])
    expect(move(['A', 'B'], 0.5, 1)).toEqual(['A', 'B'])
  })
})
