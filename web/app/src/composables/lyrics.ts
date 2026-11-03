/**
 * The index of the line being sung at `positionS`: the last line whose
 * timestamp is not after the position.
 *
 * `null` when nobody knows where the track stands (a radio no plugin tracks)
 * or when the position is still before the first line (an intro): no line is
 * highlighted rather than one that has not started yet.
 *
 * The lines come in order (the core sorts them), so the first line past the
 * position ends the scan.
 */
export function activeLine(lines: { t_ms: number }[], positionS: number | null): number | null {
  if (positionS === null) return null
  const ms = positionS * 1000
  let found: number | null = null
  for (let i = 0; i < lines.length; i++) {
    if (lines[i]!.t_ms > ms) break
    found = i
  }
  return found
}

/**
 * How long the dialog leaves the scrolling alone after the listener scrolled
 * by hand: following the song would yank the text from under the finger of
 * someone reading ahead.
 */
export const MANUAL_SCROLL_HOLD_MS = 4000
