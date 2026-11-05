import { describe, expect, it } from 'vitest'
import {
  browserOffsetSeconds, formatHourMinute, formatOffset, hourAtBrowser, sameZone, zoneChoices,
} from './timezone'

describe('sameZone', () => {
  it('treats every UTC alias as one zone', () => {
    // A Debian image links `/etc/localtime` to `Etc/UTC` while a browser in
    // UTC reports `UTC`: the same zone under two names must not raise a
    // warning nobody can act on.
    const aliases = ['UTC', 'Etc/UTC', 'Etc/Universal', 'Universal', 'Etc/Zulu', 'Zulu', 'Etc/GMT', 'GMT']
    for (const a of aliases) for (const b of aliases) expect(sameZone(a, b)).toBe(true)
  })

  it('tells different zones apart, an alias included', () => {
    expect(sameZone('Europe/Paris', 'Europe/Paris')).toBe(true)
    expect(sameZone('Europe/Paris', 'Europe/Berlin')).toBe(false)
    expect(sameZone('UTC', 'Europe/London')).toBe(false)
    expect(sameZone('Europe/London', 'Etc/UTC')).toBe(false)
  })

  it('never calls an unknown device zone the same as the browser one', () => {
    expect(sameZone(null, 'UTC')).toBe(false)
    expect(sameZone(null, 'Europe/Paris')).toBe(false)
  })
})

describe('hourAtBrowser', () => {
  it('moves the hour by the difference of the offsets', () => {
    expect(hourAtBrowser(3, 0, 3600)).toBe(4)
    expect(hourAtBrowser(3, 3600, 0)).toBe(2)
    expect(hourAtBrowser(3, 7200, 7200)).toBe(3)
  })

  it('wraps around midnight in both directions', () => {
    expect(hourAtBrowser(23, 0, 3600)).toBe(0)
    expect(hourAtBrowser(0, 3600, 0)).toBe(23)
  })

  it('keeps the half hour of a zone that is not a whole number of hours away', () => {
    // India is UTC+05:30: 03:00 UTC is 08:30 there.
    expect(hourAtBrowser(3, 0, 19_800)).toBe(8.5)
    // Kathmandu, UTC+05:45, seen from UTC: 03:00 there is 21:15 the day before.
    expect(hourAtBrowser(3, 20_700, 0)).toBe(21.25)
  })
})

describe('formatHourMinute', () => {
  it('writes a fractional hour as HH:MM on 24 h', () => {
    expect(formatHourMinute(4, true)).toBe('04:00')
    expect(formatHourMinute(8.5, true)).toBe('08:30')
    expect(formatHourMinute(21.25, true)).toBe('21:15')
    expect(formatHourMinute(0, true)).toBe('00:00')
  })

  it('writes midnight and noon as 12 on 12 h, like the log times', () => {
    expect(formatHourMinute(0, false)).toBe('12:00 AM')
    expect(formatHourMinute(12, false)).toBe('12:00 PM')
    expect(formatHourMinute(15.5, false)).toBe('3:30 PM')
  })
})

describe('formatOffset', () => {
  it('writes an offset as UTC±hh:mm', () => {
    expect(formatOffset(0)).toBe('UTC+00:00')
    expect(formatOffset(3600)).toBe('UTC+01:00')
    expect(formatOffset(-16_200)).toBe('UTC-04:30')
    expect(formatOffset(20_700)).toBe('UTC+05:45')
  })
})

describe('browserOffsetSeconds', () => {
  it('is the opposite of getTimezoneOffset, in seconds', () => {
    const d = new Date(0)
    d.getTimezoneOffset = () => -60
    expect(browserOffsetSeconds(d)).toBe(3600)
  })
})

describe('zoneChoices', () => {
  it('lists the IANA zones the browser knows', () => {
    expect(zoneChoices()).toContain('Europe/Paris')
  })
})
