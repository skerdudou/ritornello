import { describe, expect, it } from 'vitest'
import { xValues, sparklinePath, minuteTicks } from './sparkline'

describe('minuteTicks', () => {
  it('marks the full minutes of the clock, not a countdown from the end', () => {
    // Window from 30 s to 2 min 30: the full minutes falling inside are 1 min
    // and 2 min, at the quarter and three quarters. A countdown from the end
    // would have put them 30 s and 1 min 30 from the end, i.e. elsewhere.
    expect(minuteTicks([30_000, 150_000], 100)).toEqual([25, 75])
  })

  it('places nothing when no full minute falls within the window', () => {
    expect(minuteTicks([10_000, 40_000], 100)).toEqual([])
  })

  it('places a mark in a window shorter than a minute that contains one', () => {
    // 50 s → 1 min 10: twenty seconds of window, and yet a full minute in the
    // middle. The old rule, which counted elapsed minutes, showed none.
    expect(minuteTicks([50_000, 70_000], 100)).toEqual([50])
  })

  it('includes the full minutes falling on the edges', () => {
    expect(minuteTicks([0, 120_000], 100)).toEqual([0, 50, 100])
  })

  it('places nothing on a zero extent or an empty array', () => {
    expect(minuteTicks([7, 7], 100)).toEqual([])
    expect(minuteTicks([], 100)).toEqual([])
  })

  it('caps the number of ticks on an aberrant extent', () => {
    // A clock that jumps (machine without a battery or network at boot) gives
    // an absurd extent: a year covered would make half a million marks for a
    // few hundred pixels.
    const oneYear = 365 * 24 * 60 * 60 * 1000
    expect(minuteTicks([0, oneYear], 100).length).toBe(240)
  })
})

describe('xValues', () => {
  it('anchors the first point at 0 and the last at the edge', () => {
    expect(xValues([0, 5000], 100)).toEqual([0, 100])
  })

  it('spaces the points by elapsed time and not by their rank', () => {
    // The case that rank placement got wrong: three samples, the second taken
    // 2 s after the first, the third 6 s later. It belongs to the first
    // quarter of the covered time, hence to the first quarter of the width —
    // rank placement would have put it in the middle.
    expect(xValues([0, 2000, 8000], 100)).toEqual([0, 25, 100])
  })

  it('reconverges towards equidistance when the gaps become regular again', () => {
    // This is what happens as the samples taken at the old period leave the
    // buffer: nothing distinguishes the chart from an equidistant plot any
    // more.
    expect(xValues([0, 1000, 2000, 3000, 4000], 100)).toEqual([0, 25, 50, 75, 100])
  })

  it('falls back on equidistance when the extent is zero', () => {
    // Several samples in the same millisecond (or a clock frozen by fake
    // timers): equidistant for want of anything better, and above all no
    // division by zero that would fill the plot with `NaN`.
    expect(xValues([7, 7, 7], 100)).toEqual([0, 50, 100])
  })

  it('returns an empty list or a single zero below two points', () => {
    expect(xValues([], 100)).toEqual([])
    expect(xValues([42], 100)).toEqual([0])
  })
})

describe('sparklinePath', () => {
  it('draws nothing with fewer than two points', () => {
    expect(sparklinePath([], [], 30)).toBe('')
    expect(sparklinePath([42], [0], 30)).toBe('')
  })

  it('draws nothing if the xValues are not paired with the values', () => {
    // A mismatched call would draw `NaN`s: better an invisible `<path>`.
    expect(sparklinePath([0, 50, 100], [0, 100], 30)).toBe('')
  })

  it('inverts the y axis: 0 % at the bottom, 100 % at the top', () => {
    expect(sparklinePath([0, 100], [0, 100], 30)).toBe('M0.00,30.00 L100.00,0.00')
  })

  it('clamps values outside 0-100', () => {
    // A load higher than the number of cores exceeds 100 % and must not leave
    // the frame.
    expect(sparklinePath([-10, 200], [0, 100], 30)).toBe(
      sparklinePath([0, 100], [0, 100], 30),
    )
  })

  it('follows the provided xValues rather than spreading evenly', () => {
    // The same three values, placed first regularly then according to
    // irregular timestamps: only the abscissa of the middle point changes.
    expect(sparklinePath([0, 50, 100], [0, 50, 100], 30)).toBe(
      'M0.00,30.00 L50.00,15.00 L100.00,0.00',
    )
    expect(sparklinePath([0, 50, 100], xValues([0, 2000, 8000], 100), 30)).toBe(
      'M0.00,30.00 L25.00,15.00 L100.00,0.00',
    )
  })

  it('opens a second subpath after a gap in the middle of the series', () => {
    // A `null` closes the current subpath; the next present point reopens one
    // with a new `M` rather than joining it with an `L`, which would draw a
    // line over the gap.
    expect(sparklinePath([0, null, 100, 100], [0, 25, 50, 100], 30)).toBe(
      'M0.00,30.00 M50.00,0.00 L100.00,0.00',
    )
  })

  it('ignores a gap at the head of the series, without a ghost point at the edge', () => {
    expect(sparklinePath([null, 0, 100], [0, 50, 100], 30)).toBe(
      'M50.00,30.00 L100.00,0.00',
    )
  })

  it('ignores a gap at the end of the series', () => {
    expect(sparklinePath([0, 100, null], [0, 50, 100], 30)).toBe(
      'M0.00,30.00 L50.00,0.00',
    )
  })

  it('an isolated point between two gaps draws nothing, without being a special case', () => {
    // A lone `M`, with no `L` following it: consistent with the documented
    // contract of a single-point subpath, not an anomaly to handle separately.
    expect(sparklinePath([null, 50, null], [0, 50, 100], 30)).toBe('M50.00,15.00')
  })

  it('draws nothing when all values are missing', () => {
    // The case of a machine without the corresponding probe: never a curve,
    // not even an isolated `M`.
    expect(sparklinePath([null, null, null], [0, 50, 100], 30)).toBe('')
  })
})
