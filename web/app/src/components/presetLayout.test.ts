import { afterEach, describe, expect, it, vi } from 'vitest'
import { classifySwipe, LAYOUT_KEY, loadPresetLayout, revealScrollTop, savePresetLayout } from './presetLayout'

describe('classifySwipe', () => {
  it.each([
    [-50, 0, 'next'],
    [50, 0, 'previous'],
    [-49, 0, null],
    [49, 0, null],
    [-60, 31, null],
    [-60, -31, null],
    [-60, 30, 'next'],
    [60, -30, 'previous'],
    [0, -200, null],
  ])('(%d, %d) is %j', (dx, dy, expected) => {
    expect(classifySwipe(dx, dy)).toBe(expected)
  })
})

afterEach(() => {
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('loadPresetLayout', () => {
  it('reads pages when nothing is stored', () => {
    expect(loadPresetLayout()).toBe('pages')
  })

  it('reads list when list is stored', () => {
    localStorage.setItem(LAYOUT_KEY, 'list')
    expect(loadPresetLayout()).toBe('list')
  })

  it.each(['grid', '', 'LIST'])('reads pages for the stray value %j', (value) => {
    localStorage.setItem(LAYOUT_KEY, value)
    expect(loadPresetLayout()).toBe('pages')
  })

  it('reads pages, and saving does not throw, when storage throws', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('denied') })
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('denied') })
    expect(loadPresetLayout()).toBe('pages')
    expect(() => savePresetLayout('list')).not.toThrow()
  })

  it('returns what was saved', () => {
    savePresetLayout('list')
    expect(loadPresetLayout()).toBe('list')
  })
})

describe('revealScrollTop', () => {
  it('leaves a fully visible row alone', () => {
    expect(revealScrollTop(0, 200, 50, 48, 1000)).toBe(0)
  })

  it('centres a row below the frame', () => {
    expect(revealScrollTop(0, 200, 500, 48, 1000)).toBe(500 + 24 - 100)
  })

  it('clamps at the top', () => {
    expect(revealScrollTop(300, 200, 10, 48, 1000)).toBe(0)
  })

  it('clamps at the bottom', () => {
    expect(revealScrollTop(0, 200, 960, 48, 1000)).toBe(800)
  })

  it('re-centres a row partly above the top edge', () => {
    expect(revealScrollTop(100, 200, 90, 48, 1000)).toBe(90 + 24 - 100)
  })

  it('re-centres a row partly below the bottom edge', () => {
    expect(revealScrollTop(0, 200, 170, 48, 1000)).toBe(170 + 24 - 100)
  })
})
