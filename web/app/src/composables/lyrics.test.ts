import { describe, expect, it } from 'vitest'
import { activeLine } from './lyrics'

// Three lines as a sidecar `.lrc` gives them: the first one does not start at
// zero, which is what makes "before the first line" a real case (an intro).
const LINES = [{ t_ms: 2_000 }, { t_ms: 5_500 }, { t_ms: 9_000 }]

describe('activeLine', () => {
  it('is null before the first line', () => {
    expect(activeLine(LINES, 0)).toBeNull()
    expect(activeLine(LINES, 1.999)).toBeNull()
  })

  it('is the line whose timestamp the position sits exactly on', () => {
    expect(activeLine(LINES, 2)).toBe(0)
    expect(activeLine(LINES, 5.5)).toBe(1)
    expect(activeLine(LINES, 9)).toBe(2)
  })

  it('is the previous line between two timestamps', () => {
    expect(activeLine(LINES, 5.499)).toBe(0)
    expect(activeLine(LINES, 7)).toBe(1)
    // Past the last line, the last one stays current until the end.
    expect(activeLine(LINES, 600)).toBe(2)
  })

  it('is null when the position is unknown', () => {
    expect(activeLine(LINES, null)).toBeNull()
    // Not mistaken for position zero: a song whose first line starts at once
    // would otherwise highlight it on a radio nobody tracks.
    expect(activeLine([{ t_ms: 0 }, { t_ms: 3_000 }], null)).toBeNull()
  })

  it('is null for no lines at all', () => {
    expect(activeLine([], 10)).toBeNull()
  })
})
