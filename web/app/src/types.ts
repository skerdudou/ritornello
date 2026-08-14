import type { Mode } from '@ritornello/ui'

export interface PluginStatus { name: string; kind: string; connected: boolean; admin: boolean }
export interface StatusPayload { plugins: PluginStatus[]; active_source: string }
export interface AudioDevice { name: string; description: string }
export interface AudioPayload { devices: AudioDevice[]; current: string | null }
export interface LocalePayload { locales: string[]; current: string | null }
export interface ThemePayload { theme: string; mode: Mode }
export interface LogsPayload { lines: string[] }
/** Réglages de comportement, tels que les sert `GET /api/settings`. */
export interface SettingsPayload {
  volume_repeat_initial_ms: number
  volume_repeat_interval_ms: number
  start_in_standby: boolean
}
/**
 * Etat du lecteur, tel que le pousse `/api/player` : tout ce qui est volatil.
 *
 * Un seul objet, plat, pour un seul encart. `/api/status` porte a cote le
 * contrat de navigation, structurellement stable et lu une fois au montage —
 * c'est pourquoi le volume n'y est pas.
 *
 * Les champs du morceau sont optionnels : on affiche toute information
 * disponible, meme partielle. `origin` dit qui l'a fournie — `"icy"` pour ce
 * que le flux annonce lui-meme, sinon le nom du plugin `metadata` qui a gagne.
 */
export interface PlayerPayload {
  source: string
  volume: number
  muted: boolean
  standby: boolean
  /**
   * Touche numerotee correspondant a ce qui joue (preselection radio, piste cd),
   * telle que la source active l'a declaree — c'est elle que la telecommande
   * met en evidence. `null` : rien ne joue, ou rien de declare.
   */
  preset: number | null
  /**
   * Nombre de preselections que la source active declare (stations radio,
   * pistes cd), ou `null` si elle ne le declare pas — la grille retombe alors
   * sur les 9 touches nues historiques. `0` est significatif ("rien a
   * numeroter", ex. cd sans disque) et distinct de `null`.
   */
  preset_count: number | null
  artist: string | null
  title: string | null
  album: string | null
  duration_s: number | null
  origin: string | null
}
export type Command = { cmd: string; arg?: number }
export interface SystemUsage { total_kb: number; available_kb: number }
/**
 * Metriques de l'OS, telles que les sert `GET /api/system`.
 *
 * Tout champ que la machine n'expose pas vaut `null` — pas de capteur
 * thermique, pas de cpufreq, pas de sonde de sous-tension — et la vue
 * affiche « — » sans traiter cela comme une panne. Le jeu de cles, lui, est
 * stable.
 */
export interface SystemPayload {
  temperature_c: number | null
  cpu_mhz: number | null
  load: [number, number, number] | null
  cpus: number | null
  memory: SystemUsage | null
  disk: SystemUsage | null
  under_voltage: boolean | null
  uptime_s: number | null
  service_uptime_s: number
  hostname: string | null
  ip: string | null
  os: string | null
  kernel: string | null
  version: string
  can_power_off: boolean
  can_reboot: boolean
  /**
   * Compteurs cumulatifs de `/proc/stat` depuis le démarrage — jamais un
   * pourcentage : deux onglets sondant hors phase corrompraient un delta
   * calculé côté cœur. La vue les compare entre deux sondages successifs.
   */
  cpu_total_jiffies: number | null
  cpu_idle_jiffies: number | null
}
