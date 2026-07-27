import type { Mode } from '@ritornello/ui'

export interface PluginStatus { name: string; kind: string; connected: boolean; admin: boolean }
export interface StatusPayload { plugins: PluginStatus[]; active_source: string }
export interface AudioPayload { devices: string[]; current: string | null }
export interface LocalePayload { locales: string[]; current: string | null }
export interface ThemePayload { theme: string; mode: Mode }
export interface LogsPayload { lines: string[] }
/**
 * Morceau en cours, tel que le pousse `/api/now-playing`.
 *
 * Tous les champs sauf `source` sont optionnels : on affiche toute information
 * disponible, meme partielle. `origin` dit qui l'a fournie — `"icy"` pour ce
 * que le flux annonce lui-meme, sinon le nom du plugin `metadata` qui a gagne.
 */
export interface NowPlayingPayload {
  source: string
  artist: string | null
  title: string | null
  album: string | null
  duration_s: number | null
  origin: string | null
}
export type Command = { cmd: string; arg?: number }
