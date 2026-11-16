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
