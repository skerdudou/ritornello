/**
 * Abscissas of the samples in the chart's frame, proportional to their
 * **timestamp** and not to their rank.
 *
 * Equidistant points assume a constant cadence. It is not constant: the probe
 * period gets changed along the way, and the history then keeps samples
 * spaced 30 s apart next to others spaced 1 s apart. Spreading them evenly
 * would lie about time — five minutes of old history would take as much
 * width as five recent seconds, and the slope of a load rise would no longer
 * mean anything. Starting from the timestamps, the axis becomes a time axis
 * again, and the plot reconverges by itself towards equidistance as the old
 * samples leave the buffer.
 *
 * The first sample is at 0 and the last at `width`: the window is always
 * full, it is its scale that changes. See `windowMinutes`, which announces the
 * duration actually covered, measured on the same timestamps.
 *
 * Zero extent — two samples in the same millisecond, or a clock frozen by fake
 * timers —: fall back on equidistance, which beats a division by zero.
 */
export function xValues(timestamps: number[], width: number): number[] {
  const n = timestamps.length
  const start = timestamps[0]
  const end = timestamps[n - 1]
  // Undefined ⇔ empty array. The test is on the values and not on `n`,
  // because `noUncheckedIndexedAccess` does not infer from `n >= 2` that the
  // indexed accesses are safe — and a `!` assertion would hide here the only
  // thing that makes this code total.
  if (start === undefined || end === undefined) return []
  if (n === 1) return [0]
  const extent = end - start
  if (extent <= 0) return timestamps.map((_, i) => (i * width) / (n - 1))
  return timestamps.map((t) => ((t - start) / extent) * width)
}

const MINUTE_MS = 60_000

/**
 * Maximum number of ticks rendered. The real window caps well below (240
 * samples at 30 s make 120 minutes), but an aberrant timestamp — a clock that
 * jumps, commonplace on a machine without a battery or network at boot —
 * would otherwise produce thousands of elements for a chart a few hundred
 * pixels wide.
 */
const MAX_TICKS = 240

/**
 * Abscissas of the minute ticks: a mark at each **full minute of the clock**
 * (seconds at zero) falling within the covered window, rendered from left to
 * right.
 *
 * Clock minutes and not a countdown from "now": a mark then designates a real
 * instant, the one read on a watch, and two screenshots taken at different
 * moments speak of the same axis. The trade-off is that the marks **slide**
 * to the left as time passes, instead of staying still — that is the nature
 * of a fixed instant on a scrolling window, not a defect.
 *
 * The modulo is enough to find those instants: the Unix epoch itself falls on
 * a full minute and all usual time offsets are multiples of the minute, so
 * `t % 60000 == 0` means "seconds at zero" whatever the zone.
 *
 * Same scale as `xValues`, necessarily: a tick that would not share the
 * plot's scale would designate another instant than the one it claims.
 * Zero extent: no mark. A window shorter than a minute can on the contrary
 * carry one, if a full minute falls inside it.
 */
export function minuteTicks(timestamps: number[], width: number): number[] {
  const n = timestamps.length
  const start = timestamps[0]
  const end = timestamps[n - 1]
  if (start === undefined || end === undefined) return []
  const extent = end - start
  if (extent <= 0) return []
  const first = Math.ceil(start / MINUTE_MS) * MINUTE_MS
  const count = Math.min(
    Math.floor((end - first) / MINUTE_MS) + 1,
    MAX_TICKS,
  )
  if (count <= 0) return []
  return Array.from(
    { length: count },
    (_, i) => ((first + i * MINUTE_MS - start) / extent) * width,
  )
}

/**
 * Builds the `d` attribute of an SVG `<path>` for a series of percentages,
 * placed at the xValues provided by `xValues`. A `null` value marks a sample
 * whose measurement failed (for example an unreadable temperature): it opens
 * a **gap** in the plot rather than being filled in.
 *
 * All the chart's geometry lives here and in `xValues`, as pure, tested
 * functions: the view only has to pass its series. The xValues come as a
 * parameter rather than being recomputed here because the three series, the
 * hover line and the positioning of the popover must share exactly the same
 * ones — a popover shifted by one column from the plot it comments on would be
 * worse than no popover.
 *
 * Present values are clamped to 0-100 — a load higher than the number of
 * cores exceeds 100 % and must not leave the frame — and the y axis is
 * inverted: 0 % at the bottom, as one reads a chart, whereas the SVG frame has
 * its origin at the top.
 *
 * An SVG `<path>` accepts several subpaths: each `null` closes the current
 * subpath, and the next present sample reopens one with a new `M` rather than
 * continuing with an `L`. Two points on either side of the gap are thus never
 * joined by a line — the only other practicable option would be to copy the
 * last known value over the gap, which would draw a perfectly horizontal
 * plateau, indistinguishable to the eye from a real, stable measurement. A
 * visible gap says "we don't know"; a plateau would claim to know.
 *
 * Fewer than two points: empty string. A lone sample draws no line, and an
 * empty `d` is an invisible `<path>`, not an error — this is also what
 * happens for a subpath of a single point isolated between two gaps: an `M`
 * with no `L` following it, which draws nothing either, without being a
 * special case. As many xValues as values, otherwise empty string as well: a
 * mismatched call would draw `NaN`s, a silent degradation is better. All
 * values `null`: empty string too, no subpath ever opens — the case of a
 * machine without the corresponding probe.
 */
export function sparklinePath(
  values: (number | null)[],
  xs: number[],
  height: number,
): string {
  if (values.length < 2 || xs.length !== values.length) return ''
  let segment = true
  return values
    .map((v, i) => {
      if (v === null) {
        // Closes the current subpath: the next present point will reopen with
        // an `M`, not an `L` that would join it over the gap.
        segment = true
        return ''
      }
      const clamped = Math.min(100, Math.max(0, v))
      const y = height - (clamped / 100) * height
      // The `?? 0` is unreachable — both lengths have just been checked equal
      // — but it beats a `!` assertion: if someone one day relaxes that check,
      // the plot shifts instead of filling with `NaN`.
      const x = xs[i] ?? 0
      const command = segment ? 'M' : 'L'
      segment = false
      return `${command}${x.toFixed(2)},${y.toFixed(2)}`
    })
    .filter((s) => s !== '')
    .join(' ')
}
