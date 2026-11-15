import type { SleepMode } from '../types'

/** The durations the sleep timer menu offers, in minutes. */
export const SLEEP_TIMER_MINUTES = [15, 30, 45, 60, 90] as const

/**
 * The bounds of a timer's duration, in minutes: the core ignores a
 * `SetSleepTimer` outside them (`state::SLEEP_TIMER_MINUTES`), the same bounds
 * as an alarm's "turn off after".
 */
export const SLEEP_TIMER_MIN = 1
export const SLEEP_TIMER_MAX = 600

/**
 * A duration typed in the menu's free field, or `null` when the core would
 * ignore it: only a whole number of minutes within the bounds. The field binds
 * a string once edited (a number before), so both are read; `Number('2.5')`
 * or `Number('1e2')` would let through what the digits-only test refuses.
 */
export function customMinutes(value: string | number): number | null {
  const text = String(value).trim()
  if (!/^\d+$/.test(text)) return null
  const n = Number(text)
  return n >= SLEEP_TIMER_MIN && n <= SLEEP_TIMER_MAX ? n : null
}

/** What "+15 min" adds to a running timer, in minutes (`ExtendSleepTimer`). */
export const SLEEP_TIMER_EXTEND_MIN = 15

/** The three end modes, in the order the menu lists them. */
export const SLEEP_MODES: readonly SleepMode[] = ['immediate', 'fade', 'end_of_track']

/**
 * The seconds left before `endsAtUnixS`, never negative.
 *
 * Rounded **up**: the key reads `0:01` until the deadline, not `0:00` a
 * second early — the countdown of a kitchen timer.
 */
export function remainingSeconds(endsAtUnixS: number, nowMs: number): number {
  return Math.max(0, Math.ceil(endsAtUnixS - nowMs / 1000))
}

/** `mm:ss` under an hour, `h:mm:ss` from an hour up. */
export function formatRemaining(seconds: number): string {
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const s = seconds % 60
  const ss = String(s).padStart(2, '0')
  if (h === 0) return `${String(m).padStart(2, '0')}:${ss}`
  return `${h}:${String(m).padStart(2, '0')}:${ss}`
}
