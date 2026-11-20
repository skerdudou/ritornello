// The presets card can show its presets in pages of ten (the default) or as
// one scrolling list. The choice is remembered in the browser's localStorage,
// not on the device: a phone and a desk screen on the same radio have
// different sizes and different habits, so each keeps its own preference and
// nothing is sent to the core.
//
// Every storage access is guarded. localStorage can throw (private window,
// blocked site data) or come back empty; the card must then render in pages
// and keep working, the choice simply not being remembered.

export type PresetLayout = 'pages' | 'list'

export const LAYOUT_KEY = 'ritornello.presets.layout'

// Anything other than exactly 'list' reads as 'pages'.
export function loadPresetLayout(): PresetLayout {
  try {
    return localStorage.getItem(LAYOUT_KEY) === 'list' ? 'list' : 'pages'
  } catch {
    return 'pages'
  }
}

export function savePresetLayout(layout: PresetLayout): void {
  try {
    localStorage.setItem(LAYOUT_KEY, layout)
  } catch {
    // Not remembered; the layout still applies for this visit.
  }
}

// A horizontal swipe on the card turns the page. Both thresholds exist to keep
// vertical page scrolling and sloppy taps from turning pages: a drag must travel
// at least SWIPE_MIN_PX sideways and at least SWIPE_RATIO times more sideways
// than vertically (a ratio of exactly SWIPE_RATIO is accepted).
export const SWIPE_MIN_PX = 50
export const SWIPE_RATIO = 2

// Left (dx < 0) is the next page, right the previous one, like a book.
export function classifySwipe(dx: number, dy: number): 'next' | 'previous' | null {
  const ax = Math.abs(dx)
  if (ax < SWIPE_MIN_PX || ax < SWIPE_RATIO * Math.abs(dy)) return null
  return dx < 0 ? 'next' : 'previous'
}

// A page that follows the finger commits on release only past this fraction of
// the card's width; a shorter drag springs back. Width 0 (not laid out) leaves
// the swipe rule alone.
export const SNAP_FRACTION = 0.25

export function settleDirection(dx: number, dy: number, width: number): 'next' | 'previous' | null {
  const direction = classifySwipe(dx, dy)
  if (direction === null || Math.abs(dx) < SNAP_FRACTION * width) return null
  return direction
}

// Where to scroll a list frame so that a row is visible. A row already fully
// inside the frame leaves the scroll untouched (no jump while the user reads);
// otherwise the row is centred, clamped to the scrollable range.
export function revealScrollTop(
  current: number,
  frameHeight: number,
  rowTop: number,
  rowHeight: number,
  scrollHeight: number,
): number {
  if (rowTop >= current && rowTop + rowHeight <= current + frameHeight) return current
  const centred = rowTop + rowHeight / 2 - frameHeight / 2
  return Math.max(0, Math.min(centred, scrollHeight - frameHeight))
}
