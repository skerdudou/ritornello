import { api, applyTheme, DEFAULT_MODE, DEFAULT_PRESET, presets, toast, type Mode } from '@ritornello/ui'
import { ref } from 'vue'
import { readBootTheme } from '../boot'
import type { ThemePayload } from '../types'

// Module-level state: the theme is unique for the page, a singleton is simpler
// than a `provide`/`inject` threaded through every component.
const theme = ref(DEFAULT_PRESET)
const mode = ref<Mode>(DEFAULT_MODE)

/**
 * Called by `main.ts` **before** mounting: the first render is already in the
 * right colours (no flicker).
 */
export function initTheme(): void {
  const choice = readBootTheme()
  theme.value = choice.theme
  mode.value = choice.mode
  applyTheme(choice.theme, choice.mode)
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
    applyTheme(t, m)
    theme.value = t
    mode.value = m
    const err = await api.put('/api/theme', { theme: t, mode: m })
    if (err) toast.error(err)
  }

  return {
    theme,
    mode,
    set,
    toggleMode: () => set({ mode: mode.value === 'dark' ? 'light' : 'dark' }),
  }
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
