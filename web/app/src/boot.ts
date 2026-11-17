import { DEFAULT_MODE, DEFAULT_PRESET, MODE_CHOICES, type ModeChoice } from '@ritornello/ui'
import type { ThemePayload } from './types'

declare global {
  interface Window {
    __RITORNELLO_THEME__?: { theme?: string; mode?: string }
  }
}

// The core injects the persisted choice directly into the shell it serves:
// the theme is therefore applied from the very first render, without waiting
// for a `GET /api/theme` round trip — no flicker. The core only carries two
// strings; it knows no color.
export function readBootTheme(win: Window = window): ThemePayload {
  const raw = win.__RITORNELLO_THEME__
  const known = MODE_CHOICES.find((m) => m === raw?.mode)
  const mode: ModeChoice = known ?? DEFAULT_MODE
  return { theme: raw?.theme || DEFAULT_PRESET, mode }
}
