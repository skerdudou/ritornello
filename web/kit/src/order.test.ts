import { describe, expect, it } from 'vitest'
import { move, moveAmong } from './order'

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

describe('moveAmong', () => {
  it('equals move() when every item is movable', () => {
    expect(moveAmong(['a', 'b', 'c'], () => true, 2, 0)).toEqual(['c', 'a', 'b'])
  })
  it('leaves an unmovable item at its index', () => {
    // positions among movables: a=0, c=1, d=2
    expect(moveAmong(['a', 'B', 'c', 'd'], (x) => x !== 'B', 2, 0)).toEqual(['d', 'B', 'a', 'c'])
  })
  it('ignores out-of-range and equal positions', () => {
    const l = ['a', 'B', 'c']
    expect(moveAmong(l, (x) => x !== 'B', 0, 2)).toEqual(l)
    expect(moveAmong(l, (x) => x !== 'B', 1, 1)).toEqual(l)
  })
})
