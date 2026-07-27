import { DEFAULT_MODE, DEFAULT_PRESET, type Mode } from '@ritornello/ui'
import type { ThemePayload } from './types'

declare global {
  interface Window {
    __RITORNELLO_THEME__?: { theme?: string; mode?: string }
  }
}

// Le coeur injecte le choix persiste directement dans le shell qu'il sert :
// le theme est donc applique des le premier rendu, sans attendre un
// aller-retour `GET /api/theme` — pas de clignotement. Le coeur ne
// transporte que deux chaines ; il ne connait aucune couleur.
export function readBootTheme(win: Window = window): ThemePayload {
  const brut = win.__RITORNELLO_THEME__
  const mode: Mode = brut?.mode === 'dark' || brut?.mode === 'light' ? brut.mode : DEFAULT_MODE
  return { theme: brut?.theme || DEFAULT_PRESET, mode }
}
