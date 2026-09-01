import type { DateFormat } from '../types'

/**
 * Log lines retained by a filter query: substring, case-insensitive, input
 * order preserved.
 *
 * The order is a contract, not an accident: `/api/logs` returns the most
 * recent lines first, and a filter that re-sorted would reverse that
 * chronology without the caller having asked for it.
 *
 * An empty query — or one made of spaces only, which an input field produces
 * all the time — returns the whole list rather than no line: a field one has
 * just cleared must return what was visible before typing into it.
 */
export function filterLines(lines: string[], query: string): string[] {
  const q = query.trim().toLowerCase()
  if (!q) return lines
  return lines.filter((l) => l.toLowerCase().includes(q))
}


const twoDigits = (n: number) => String(n).padStart(2, '0')

/**
 * A date written the way the device is set to write it.
 *
 * A closed choice and not `Intl`: the rendering of `Intl` depends on the
 * browser's locale *and* on the engine, so it cannot be tested stably and it
 * would contradict the setting — it is precisely that setting that decides,
 * not the machine of whoever is looking.
 */
export function formatDate(d: Date, format: DateFormat): string {
  const year = d.getFullYear()
  const month = twoDigits(d.getMonth() + 1)
  const day = twoDigits(d.getDate())
  if (format === 'year_month_day') return `${year}-${month}-${day}`
  if (format === 'month_day_year') return `${month}/${day}/${year}`
  return `${day}/${month}/${year}`
}

/**
 * A log time: with seconds, unlike the standby clock. Two lines emitted in
 * the same minute are commonplace in a log, and the order is the main
 * information there.
 *
 * On 12 h, midnight is written `12:00:00 AM` and noon `12:00:00 PM` — the
 * same convention as the console display, and for the same reason: `0:00 AM`
 * exists nowhere.
 */
export function formatTime(d: Date, on24h: boolean): string {
  const minutes = twoDigits(d.getMinutes())
  const seconds = twoDigits(d.getSeconds())
  if (on24h) return `${twoDigits(d.getHours())}:${minutes}:${seconds}`
  const h = d.getHours()
  const suffix = h < 12 ? 'AM' : 'PM'
  return `${h % 12 === 0 ? 12 : h % 12}:${minutes}:${seconds} ${suffix}`
}

/**
 * Rewrites the timestamp at the head of a log line in the configured format,
 * and **in the browser's time zone**.
 *
 * The core logs in UTC (`2026-08-28T12:18:32.016060Z`), which is the right
 * choice for a file but reads poorly when looking for "what happened five
 * minutes ago". The time zone comes from the browser and not from a setting:
 * it is that of whoever is looking, which stays right for a travelling phone,
 * where one more setting could contradict the device.
 *
 * A line without a recognizable timestamp is rendered **as is**: the core's
 * buffer today only holds its own lines, but a line we cannot read must stay
 * readable rather than be truncated.
 */
export function lineDate(line: string, format: DateFormat, on24h: boolean): string {
  const found = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)\s+/.exec(line)
  if (!found) return line
  const d = new Date(found[1]!)
  if (Number.isNaN(d.getTime())) return line
  return `${formatDate(d, format)} ${formatTime(d, on24h)} ${line.slice(found[0].length)}`
}
