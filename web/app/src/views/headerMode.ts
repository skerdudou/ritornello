/**
 * What the Player card's header offers to change source.
 *
 * - `icons`: one key per source, the input selector of an amplifier — a
 *   single click to the wanted source, where the cycle key needs as many
 *   presses as there are notches to it.
 * - `cycle`: the `SourceCycle` key, for when the icon keys do not fit, and
 *   for when the page does not know yet: the source list has not been read
 *   (a failed `/api/presets` must not leave the page with no way at all to
 *   change source) or the header has not been measured.
 * - `none`: a single source, nothing to choose between.
 */
export type HeaderMode = 'icons' | 'cycle' | 'none'

/** An icon key's side, in px: the kit's `icon-sm` button is `size-8`. */
export const KEY = 32
/** The gap between two keys of the header, in px: `gap-1`. */
export const GAP = 4
/**
 * The standby badge's width, in px: the widest rendered width of the
 * `[data-standby]` badge (kit `Badge`, `secondary`) across the five shipped
 * languages and every theme font, rounded up.
 *
 * Measured in Chromium (Playwright 1.63, Windows) on 2026-10-10, the badge
 * rendered with the built stylesheet and each text of the `standby` key:
 * STANDBY (en, de, it), VEILLE (fr), EN ESPERA (es), in each `font-sans` of
 * the theme presets (Google Fonts, weight 500) and in `system-ui`. The
 * widest is "EN ESPERA": 94.42 px in Libre Baskerville; 78.22 px in
 * `system-ui` (Segoe UI), 83.72 px in Inter, 88.61 px in Montserrat, 82.81
 * px in the monospace fonts. A wider text in a future pack only makes the
 * keys overflow by the difference in standby; re-measure when one is added.
 */
export const STANDBY_BADGE = 95

export function headerMode(input: {
  /** How many sources the list holds. */
  sources: number
  /** Whether `/api/presets` has been read at least once. */
  listRead: boolean
  /** The header's content-box width in px, `null` before the first measure. */
  available: number | null
  /** The standby badge shares the header's row with the keys. */
  standby: boolean
  /**
   * Whether the active source is empty or in the list. The core keeps the
   * active source while mpv plays after its plugin died, so after a reload the
   * active one can be absent from the list: no icon key would be pressed and
   * the header would name no source. The cycle mode shows the pill.
   */
  activeListed: boolean
}): HeaderMode {
  const { sources, listRead, available, standby, activeListed } = input
  if (listRead && sources <= 1) return 'none'
  if (!listRead || available === null) return 'cycle'
  if (!activeListed) return 'cycle'
  // The keys and the gaps between them, then the gap and the standby key that
  // always close the row, then the standby badge when it shows.
  let needed = sources * KEY + (sources - 1) * GAP + GAP + KEY
  if (standby) needed += STANDBY_BADGE + GAP
  return needed <= available ? 'icons' : 'cycle'
}
