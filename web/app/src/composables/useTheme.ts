import {
  api,
  applyTheme,
  DEFAULT_MODE,
  DEFAULT_PRESET,
  MODE_CHOICES,
  presets,
  resolveMode,
  toast,
  type ModeChoice,
} from '@ritornello/ui'
import { computed, ref } from 'vue'
import { readBootTheme } from '../boot'
import type { ThemePayload } from '../types'

// Module-level state: the theme is unique for the page, a singleton is simpler
// than a `provide`/`inject` threaded through every component.
const theme = ref(DEFAULT_PRESET)
/** The device's choice, as stored: `auto` stays `auto` here. */
const mode = ref<ModeChoice>(DEFAULT_MODE)
/** This browser's system preference, kept current by `followSystem`. */
const prefersDark = ref(false)
/** What is painted: `mode` with `auto` resolved for this browser. */
const effectiveMode = computed(() => resolveMode(mode.value, prefersDark.value))

const SYSTEM_DARK = '(prefers-color-scheme: dark)'
let systemQuery: MediaQueryList | null = null

function onSystemChange(e: MediaQueryListEvent): void {
  prefersDark.value = e.matches
  // A fixed mode does not care; only `auto` repaints when the system turns
  // dark at sunset while the page stays open.
  if (mode.value === 'auto') applyTheme(theme.value, effectiveMode.value)
}

/**
 * Subscribes once per page to the system preference. Re-entrant (the tests
 * call `initTheme` before each case): the previous subscription is dropped
 * first, so a change never repaints twice. Without `matchMedia` (jsdom), the
 * system counts as light — the same default as the device's.
 */
function followSystem(win: Window): void {
  systemQuery?.removeEventListener('change', onSystemChange)
  systemQuery = typeof win.matchMedia === 'function' ? win.matchMedia(SYSTEM_DARK) : null
  prefersDark.value = systemQuery?.matches ?? false
  systemQuery?.addEventListener('change', onSystemChange)
}

/**
 * Called by `main.ts` **before** mounting: the first render is already in the
 * right colours (no flicker). `auto` costs nothing more here: `matchMedia`
 * answers synchronously, so it is resolved before the first paint like a
 * fixed mode — the core, which never sees the browser, could not do it.
 */
export function initTheme(win: Window = window): void {
  const choice = readBootTheme(win)
  theme.value = choice.theme
  mode.value = choice.mode
  followSystem(win)
  applyTheme(choice.theme, effectiveMode.value)
}

export function useTheme() {
  /**
   * Apply **first**, persist afterwards: the setting is an appearance choice,
   * the user must see it immediately. If persistence fails, we report it
   * without rolling back — cancelling silently would give a UI that seems to
   * ignore clicks.
   */
  async function set(next: Partial<ThemePayload>): Promise<void> {
    const t = next.theme ?? theme.value
    const m = next.mode ?? mode.value
    theme.value = t
    mode.value = m
    applyTheme(t, effectiveMode.value)
    const err = await api.put('/api/theme', { theme: t, mode: m })
    if (err) toast.error(err)
  }

  /** Light, dark, auto, and round again: the order of `MODE_CHOICES`. */
  function cycleMode(): Promise<void> {
    const i = MODE_CHOICES.indexOf(mode.value)
    return set({ mode: MODE_CHOICES[(i + 1) % MODE_CHOICES.length] })
  }

  return { theme, mode, effectiveMode, set, cycleMode }
}

/**
 * Filter by label **or** identifier, case-insensitive. Sorted by label so
 * that the grid of the popover is stable and browsable.
 */
export function filterPresets(query: string): Array<{ id: string; label: string }> {
  const q = query.trim().toLowerCase()
  return Object.entries(presets)
    .map(([id, p]) => ({ id, label: p.label }))
    .filter(({ id, label }) => !q || label.toLowerCase().includes(q) || id.includes(q))
    .sort((a, b) => a.label.localeCompare(b.label))
}
