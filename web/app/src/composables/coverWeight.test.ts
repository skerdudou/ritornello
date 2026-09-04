import { describe, expect, it } from 'vitest'
import { BYTES_PER_PIXEL, predictedThumbnailBytes } from './coverWeight'

const KIO = 1024

describe('predictedThumbnailBytes', () => {
  // The table's three points come from the Rust bench
  // `cover::tests::the_weight_rule_of_a_thumbnail`, measured over 78 real
  // covers in release mode. This test is what stops the table from silently
  // drifting away from the measurement that justifies it.
  it('reproduces the measured p50 at the product defaults', () => {
    // 640 px, q85: 98 KiB measured.
    expect(Math.round(predictedThumbnailBytes(640, 85) / KIO)).toBe(98)
  })

  it('follows the measured direction of quality', () => {
    // Measured: 73 KiB at q75, 98 at q85, 120 at q90. The model must be
    // monotonically increasing in quality — a mistyped table that swapped
    // two rows would fail this.
    const q75 = predictedThumbnailBytes(640, 75)
    const q85 = predictedThumbnailBytes(640, 85)
    const q90 = predictedThumbnailBytes(640, 90)
    expect(q75).toBeLessThan(q85)
    expect(q85).toBeLessThan(q90)
  })

  it('interpolates between two measured qualities', () => {
    const q80 = predictedThumbnailBytes(640, 80)
    expect(q80).toBeGreaterThan(predictedThumbnailBytes(640, 75))
    expect(q80).toBeLessThan(predictedThumbnailBytes(640, 85))
  })

  it('clamps outside the measured range instead of extrapolating', () => {
    // q40 is the setting's low bound and q100 its high one, and the bench
    // measured neither. Extrapolating linearly would give a negative density
    // below q≈53 — a negative weight shown to the user. Clamping is what
    // forbids that.
    expect(predictedThumbnailBytes(640, 40)).toBe(predictedThumbnailBytes(640, 75))
    expect(predictedThumbnailBytes(640, 100)).toBe(predictedThumbnailBytes(640, 90))
    expect(predictedThumbnailBytes(640, 40)).toBeGreaterThan(0)
  })

  it('grows with the square of the edge', () => {
    // Doubling the edge quadruples the pixels, hence the weight.
    const small = predictedThumbnailBytes(320, 85)
    const large = predictedThumbnailBytes(640, 85)
    expect(large / small).toBeCloseTo(4, 5)
  })

  it('returns zero on a value a number input can actually produce', () => {
    // Emptying a box to retype it is an ordinary keystroke, and `Number('')`
    // is 0, not NaN. Without this case, the page would show "about 0 KiB",
    // and the estimate would divide by zero.
    expect(predictedThumbnailBytes(0, 85)).toBe(0)
    expect(predictedThumbnailBytes(640, 0)).toBe(0)
    expect(predictedThumbnailBytes(Number.NaN, 85)).toBe(0)
    expect(predictedThumbnailBytes(-640, 85)).toBe(0)
  })

  it('keeps the table anchored to the bench', () => {
    // The production change this would catch: editing the table without
    // rerunning the bench. The three pairs are the ones from the report.
    expect(BYTES_PER_PIXEL).toEqual([
      [75, 0.18],
      [85, 0.245],
      [90, 0.3],
    ])
  })
})
