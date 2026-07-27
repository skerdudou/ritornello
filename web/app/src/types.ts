import type { Mode } from '@ritornello/ui'

export interface PluginStatus { name: string; kind: string; connected: boolean; admin: boolean }
export interface StatusPayload { plugins: PluginStatus[]; active_source: string }
export interface AudioPayload { devices: string[]; current: string | null }
export interface LocalePayload { locales: string[]; current: string | null }
export interface ThemePayload { theme: string; mode: Mode }
export interface LogsPayload { lines: string[] }
export type Command = { cmd: string; arg?: number }
