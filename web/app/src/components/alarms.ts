import type { Alarm, Weekday } from '../types'

/**
 * The order the day toggles and the chips are drawn in: Monday first, the
 * week the owner's alarms are thought in. Only the display order — the wire
 * list is a set, and `daysInWeekOrder` re-sorts whatever the core sent.
 */
export const WEEK: readonly Weekday[] = [
  'monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday',
]

/** Bounds the core enforces (`settings_validation.rs`); the dialog mirrors them. */
export const ALARM_SLEEP_MIN = 1
export const ALARM_SLEEP_MAX = 600

/** What a new alarm opens on: weekday mornings, resuming, no sleep timer. */
export const NEW_ALARM_HOUR = 7
export const NEW_ALARM_MINUTE = 0
export const NEW_ALARM_VOLUME = 30
export const NEW_ALARM_DAYS: readonly Weekday[] = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday']
/** The duration "turn off after" proposes when it is switched on. */
export const DEFAULT_SLEEP_AFTER_MIN = 60

/**
 * The id a new alarm takes: one above the highest, 1 for the first. Never a
 * reused gap: the core only needs uniqueness, and a freed id coming back
 * would make "the alarm that just fired" ambiguous in a log.
 */
export function nextAlarmId(alarms: readonly Alarm[]): number {
  return alarms.reduce((max, a) => Math.max(max, a.id), 0) + 1
}

/** A fresh alarm for the "Add" gesture. Enabled: adding one means wanting it. */
export function newAlarm(alarms: readonly Alarm[]): Alarm {
  return {
    id: nextAlarmId(alarms),
    enabled: true,
    hour: NEW_ALARM_HOUR,
    minute: NEW_ALARM_MINUTE,
    days: [...NEW_ALARM_DAYS],
    target: { kind: 'resume' },
    volume: NEW_ALARM_VOLUME,
  }
}

export function daysInWeekOrder(days: readonly Weekday[]): Weekday[] {
  return WEEK.filter((d) => days.includes(d))
}

/**
 * The short name of a day, for the toggles and the chips. A switch of literal
 * keys rather than a built one, so `i18nKeysUsed.test.ts` sees every key.
 */
export function shortDayLabel(t: (key: string) => string, d: Weekday): string {
  switch (d) {
    case 'monday': return t('weekday_short_monday')
    case 'tuesday': return t('weekday_short_tuesday')
    case 'wednesday': return t('weekday_short_wednesday')
    case 'thursday': return t('weekday_short_thursday')
    case 'friday': return t('weekday_short_friday')
    case 'saturday': return t('weekday_short_saturday')
    default: return t('weekday_short_sunday')
  }
}

/** A deep copy, so the dialog edits a draft and never the saved list. */
export function cloneAlarm(a: Alarm): Alarm {
  const copy: Alarm = { ...a, days: [...a.days], target: { ...a.target } }
  if (a.sleep_after) copy.sleep_after = { ...a.sleep_after }
  else delete copy.sleep_after
  return copy
}
