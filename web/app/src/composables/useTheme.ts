import { api, applyTheme, DEFAULT_MODE, DEFAULT_PRESET, presets, toast, type Mode } from '@ritornello/ui'
import { ref } from 'vue'
import { readBootTheme } from '../boot'
import type { ThemePayload } from '../types'

// État au niveau du module : le thème est unique pour la page, un singleton
// est plus simple qu'un `provide`/`inject` traversé par tous les composants.
const theme = ref(DEFAULT_PRESET)
const mode = ref<Mode>(DEFAULT_MODE)

/**
 * Appelée par `main.ts` **avant** le montage : le premier rendu est déjà dans
 * les bonnes couleurs (aucun clignotement).
 */
export function initTheme(): void {
  const choix = readBootTheme()
  theme.value = choix.theme
  mode.value = choix.mode
  applyTheme(choix.theme, choix.mode)
}

export function useTheme() {
  /**
   * Applique **d'abord**, persiste ensuite : le réglage est un choix
   * d'apparence, l'utilisateur doit le voir immédiatement. Si la persistance
   * échoue, on le signale sans revenir en arrière — annuler silencieusement
   * donnerait une IHM qui semble ignorer les clics.
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
 * Filtre par libellé **ou** identifiant, insensible à la casse. Trié par
 * libellé pour que la grille de la popin soit stable et parcourable.
 */
export function filterPresets(query: string): Array<{ id: string; label: string }> {
  const q = query.trim().toLowerCase()
  return Object.entries(presets)
    .map(([id, p]) => ({ id, label: p.label }))
    .filter(({ id, label }) => !q || label.toLowerCase().includes(q) || id.includes(q))
    .sort((a, b) => a.label.localeCompare(b.label))
}
