/**
 * What a thumbnail will weigh, predicted from the two settings that decide it.
 *
 * **Why this exists at all.** The page used to ask the user for a "rendered
 * ceiling" in kibibytes, a figure nobody could know: they choose an edge and a
 * quality, and no amount of staring at those two tells you how many bytes the
 * encoder produces. This module turns the question round — the two settings
 * are the input, the weight is shown as a consequence.
 *
 * **It lives here and not in the core, deliberately.** It exists to explain,
 * and no decision depends on it: the safety net in `cover.rs` is derived from
 * the edge alone (`Rendition::net`) precisely so that this model needs no
 * second copy in Rust, with two versions to drift apart.
 */

/**
 * Bytes per pixel of the produced JPEG, by quality — measured, not derived.
 *
 * From `cover::tests::the_weight_rule_of_a_thumbnail`, run in release over 78
 * covers of a real library: at a 640 px edge the p50 output was 73 KiB at q75,
 * 98 KiB at q85 and 120 KiB at q90. Divided by 640² pixels, that is the three
 * densities below.
 *
 * Only three points, and that is honest: the bench measured three. Between
 * them the model interpolates; outside them it clamps rather than
 * extrapolates — a linear extrapolation goes negative below q≈53, and the
 * quality setting bottoms out at 40.
 */
export const BYTES_PER_PIXEL = [
  [75, 0.18],
  [85, 0.245],
  [90, 0.3],
] as const satisfies ReadonlyArray<readonly [number, number]>

/** Bytes per pixel at `quality`, interpolated between measured points. */
function bytesPerPixel(quality: number): number {
  const first = BYTES_PER_PIXEL[0]
  const last = BYTES_PER_PIXEL[BYTES_PER_PIXEL.length - 1]!
  if (quality <= first[0]) return first[1]
  if (quality >= last[0]) return last[1]
  for (let i = 1; i < BYTES_PER_PIXEL.length; i += 1) {
    const [q1, b1] = BYTES_PER_PIXEL[i]!
    if (quality <= q1) {
      const [q0, b0] = BYTES_PER_PIXEL[i - 1]!
      return b0 + ((b1 - b0) * (quality - q0)) / (q1 - q0)
    }
  }
  return last[1]
}

/**
 * Predicted weight of one thumbnail, in bytes. `0` when either input is not a
 * usable positive number.
 *
 * Zero rather than a guess: emptying a number input to retype it is an
 * ordinary keystroke, and `Number('')` is `0`, not `NaN`. Callers must treat
 * `0` as "no figure to show yet" — never divide by it.
 *
 * Square edge, so an upper bound in the general case: `image::thumbnail`
 * preserves the aspect ratio, so a non-square cover has fewer pixels than
 * `edge²`. Album art is overwhelmingly square, and erring high on a figure
 * that feeds a memory estimate is the safe direction.
 */
export function predictedThumbnailBytes(edgePx: number, quality: number): number {
  if (!Number.isFinite(edgePx) || !Number.isFinite(quality) || edgePx <= 0 || quality <= 0) {
    return 0
  }
  return Math.round(edgePx * edgePx * bytesPerPixel(quality))
}
