import { describe, expect, it } from 'vitest'
import { lineDate, filterLines, formatDate, formatTime } from './log'

const LINES = [
  'WARN plugin radio unavailable',
  'ERROR mpv socket closed',
  'WARN CIFS mount timed out',
]

describe('filterLines', () => {
  it('returns everything when the query is empty', () => {
    expect(filterLines(LINES, '')).toEqual(LINES)
  })

  it('ignores the spaces around the query', () => {
    expect(filterLines(LINES, '   ')).toEqual(LINES)
    expect(filterLines(LINES, '  mpv  ')).toEqual(['ERROR mpv socket closed'])
  })

  it('filters by case-insensitive substring', () => {
    expect(filterLines(LINES, 'WARN')).toEqual([LINES[0], LINES[2]])
    expect(filterLines(LINES, 'warn')).toEqual([LINES[0], LINES[2]])
    expect(filterLines(LINES, 'CiFs')).toEqual(['WARN CIFS mount timed out'])
  })

  it('returns an empty array without a match', () => {
    expect(filterLines(LINES, 'zzz')).toEqual([])
  })

  it('preserves the order received', () => {
    // `/api/logs` already returns the most recent first: the filter must not
    // re-sort, or it would reverse that chronology.
    expect(filterLines(LINES, 'o')).toEqual([LINES[0], LINES[1], LINES[2]])
  })
})

describe('the dating of log lines', () => {
  it('rewrites the timestamp in the configured format, and leaves the rest intact', () => {
    // The core logs in UTC; `formatDate`/`formatTime` render the **local**
    // time, so the expected value is built with them rather than hard-coded:
    // the CI does not run in the workshop's time zone, and a literal would be
    // wrong there half of the year.
    const d = new Date('2026-08-28T12:18:32.016060Z')
    const expected = `${formatDate(d, 'day_month_year')} ${formatTime(d, true)} WARN something`
    expect(
      lineDate('2026-08-28T12:18:32.016060Z WARN something', 'day_month_year', true),
    ).toBe(expected)
  })

  it('renders as is a line without a recognizable timestamp', () => {
    // The core's buffer today only holds its own lines, but a line we cannot
    // read must stay readable rather than truncated.
    for (const line of ['no date here', '', '28/08/2026 already dated']) {
      expect(lineDate(line, 'year_month_day', false)).toBe(line)
    }
  })

  it('writes the three requested date orders', () => {
    const d = new Date(2026, 11, 31, 13, 5, 9)
    expect(formatDate(d, 'day_month_year')).toBe('31/12/2026')
    expect(formatDate(d, 'year_month_day')).toBe('2026-12-31')
    expect(formatDate(d, 'month_day_year')).toBe('12/31/2026')
  })

  it('writes both time formats, midnight and noon included', () => {
    // The two bounds the Anglo-Saxon convention treats apart: a `0:00 AM`
    // exists nowhere, and noon is `12:00 PM`.
    expect(formatTime(new Date(2026, 0, 1, 0, 0, 0), true)).toBe('00:00:00')
    expect(formatTime(new Date(2026, 0, 1, 0, 0, 0), false)).toBe('12:00:00 AM')
    expect(formatTime(new Date(2026, 0, 1, 12, 0, 0), false)).toBe('12:00:00 PM')
    expect(formatTime(new Date(2026, 0, 1, 13, 5, 9), false)).toBe('1:05:09 PM')
    expect(formatTime(new Date(2026, 0, 1, 13, 5, 9), true)).toBe('13:05:09')
  })
})
