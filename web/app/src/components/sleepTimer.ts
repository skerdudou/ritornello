import type { SleepMode } from '../types'

/** The durations the sleep timer menu offers, in minutes. */
export const SLEEP_TIMER_MINUTES = [15, 30, 45, 60, 90] as const

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
