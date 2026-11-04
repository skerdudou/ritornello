/**
 * Time-zone helpers shared by the Time card (System page) and the update-hour
 * warning (Configuration page). Pure functions, apart from the two that read
 * the browser — which is the whole point of them, and why the tests pin them.
 *
 * The device's zone and offset come from `/api/system`: the core reads its
 * own `/etc/localtime`, so these helpers never guess the device side.
 */

/** The browser's IANA zone, e.g. `Europe/Paris`. */
export function browserZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone
}

/**
 * The names under which the same UTC goes. A Debian image links
 * `/etc/localtime` to `Etc/UTC` while a browser set to UTC reports `UTC`:
 * compared as strings they differ, and the page would warn about a gap of
 * zero hours that nobody can act on.
 */
const UTC_ALIASES = new Set([
  'UTC', 'Etc/UTC', 'Etc/Universal', 'Universal', 'Etc/Zulu', 'Zulu', 'Etc/GMT', 'GMT',
])

function normalise(zone: string): string {
  return UTC_ALIASES.has(zone) ? 'UTC' : zone
}

/**
 * Are the device and the browser on the same zone? An unknown device zone
 * (`null`) is never the same: the page cannot vouch for it, and saying so is
 * the safer side of the warning.
 *
 * By name, not by current offset: Paris and Berlin agree today, but nothing
 * promises two zones keep agreeing across their next rule change.
 */
export function sameZone(device: string | null, browser: string): boolean {
  if (device === null) return false
  return normalise(device) === normalise(browser)
}

/** The browser's offset from UTC at `at`, in seconds east of UTC (the sign
 *  the core uses for `utc_offset_seconds`, the opposite of
 *  `getTimezoneOffset`'s minutes west). */
export function browserOffsetSeconds(at: Date): number {
  return -at.getTimezoneOffset() * 60
}

/**
 * The browser-side hour of `deviceHour` on the device, as a number in
 * `[0, 24)`: fractional when the two offsets are not a whole number of hours
 * apart (India's `+05:30` makes 03:00 UTC read 08:30, so `8.5`). The caller
 * formats it with `formatHourMinute`.
 *
 * Both offsets are taken *now*, not at the next occurrence of the hour: on
 * the night a zone changes its clocks the answer is an hour off, for that
 * night only. The warning exists to say "these are not the same clock", which
 * that approximation does not change.
 */
export function hourAtBrowser(deviceHour: number, deviceOffset: number, browserOffset: number): number {
  const seconds = deviceHour * 3600 - deviceOffset + browserOffset
  const day = 24 * 3600
  return (((seconds % day) + day) % day) / 3600
}

const twoDigits = (n: number) => String(n).padStart(2, '0')

/**
 * A fractional hour in `[0, 24)` written `HH:MM` on 24 h, or `h:MM AM/PM` on
 * 12 h — midnight `12:00 AM` and noon `12:00 PM`, the convention of the log
 * times (`views/log.ts`) and of the console display.
 */
export function formatHourMinute(hour: number, on24h: boolean): string {
  const total = Math.round(hour * 60)
  const h = Math.floor(total / 60) % 24
  const m = twoDigits(total % 60)
  if (on24h) return `${twoDigits(h)}:${m}`
  const suffix = h < 12 ? 'AM' : 'PM'
  return `${h % 12 === 0 ? 12 : h % 12}:${m} ${suffix}`
}

/** `UTC+01:00`, `UTC-04:30`: what names a zone the device could not name. */
export function formatOffset(seconds: number): string {
  const sign = seconds < 0 ? '-' : '+'
  const minutes = Math.round(Math.abs(seconds) / 60)
  return `UTC${sign}${twoDigits(Math.floor(minutes / 60))}:${twoDigits(minutes % 60)}`
}

/**
 * Every IANA zone the browser knows, or `[]` on an engine without
 * `Intl.supportedValuesOf` (Safari before 15.4): the picker then offers only
 * the device's current zone and the browser's own, which still covers the
 * one change anyone makes from a living room.
 */
export function zoneChoices(): string[] {
  const intl = Intl as { supportedValuesOf?: (key: 'timeZone') => string[] }
  return typeof intl.supportedValuesOf === 'function' ? intl.supportedValuesOf('timeZone') : []
}
