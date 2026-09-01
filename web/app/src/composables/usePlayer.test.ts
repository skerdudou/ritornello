import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import { formatDuration, formatPosition, nothingToShow, usePlayer } from './usePlayer'

/** Complete state, from which each test removes what it wants to test. */
function state(partial: Partial<PlayerPayload> = {}): PlayerPayload {
  return {
    source: 'radio',
    volume: 60,
    muted: false,
    standby: false,
    preset: null,
    preset_count: null,
    preset_name: null,
    status: null,
    overlay: null,
    artist: 'Shaka Ponk',
    title: 'Wanna Get Free',
    album: null,
    duration_s: 214,
    origin: 'ouifm-metas',
    cover_href: null,
    cover_origin: null,
    position_s: null,
    seekable: false,
    can_eject: false,
    ...partial,
  }
}

/** Fake `EventSource`: keeps the instances and allows pushing frames. */
class FakeEventSource {
  static instances: FakeEventSource[] = []
  onmessage: ((e: MessageEvent) => void) | null = null
  closed = false
  constructor(public url: string) {
    FakeEventSource.instances.push(this)
  }
  close() {
    this.closed = true
  }
  push(data: unknown) {
    this.onmessage?.({ data: typeof data === 'string' ? data : JSON.stringify(data) } as MessageEvent)
  }
}

describe('formatDuration', () => {
  it('formats as minutes and seconds', () => {
    expect(formatDuration(214)).toBe('3:34')
    expect(formatDuration(60)).toBe('1:00')
    expect(formatDuration(9)).toBe('0:09')
    expect(formatDuration(3600)).toBe('60:00')
  })

  it('treats any unusable value as unknown', () => {
    // These values come from a third party: better to display nothing than "-1:59".
    expect(formatDuration(null)).toBeNull()
    expect(formatDuration(undefined)).toBeNull()
    expect(formatDuration(0)).toBeNull()
    expect(formatDuration(-5)).toBeNull()
    expect(formatDuration(Number.NaN)).toBeNull()
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBeNull()
  })
})

describe('formatPosition', () => {
  // `formatDuration` refuses values <= 0, which is right for a duration and
  // wrong for a position: `0:00` is a perfectly legitimate instant. Two
  // functions rather than a relaxation of the first one, which would bring
  // back "0:00" where the refusal was useful.
  it('accepts zero', () => {
    expect(formatPosition(0)).toBe('0:00')
  })
  it('formats minutes and seconds', () => {
    expect(formatPosition(87)).toBe('1:27')
    expect(formatPosition(3725)).toBe('62:05')
  })
  it('returns null on an absence', () => {
    expect(formatPosition(null)).toBeNull()
    expect(formatPosition(undefined)).toBeNull()
    expect(formatPosition(-1)).toBeNull()
    expect(formatPosition(Number.NaN)).toBeNull()
    expect(formatPosition(Number.POSITIVE_INFINITY)).toBeNull()
  })
})

describe('nothingToShow', () => {
  it('accepts any partial information', () => {
    // Owner's decision: we display everything that is available.
    expect(nothingToShow(state())).toBe(false)
    expect(nothingToShow(state({ artist: null }))).toBe(false)
    expect(nothingToShow(state({ title: null }))).toBe(false)
    expect(nothingToShow(state({ artist: null, title: null, album: 'Kind of Blue' }))).toBe(false)
  })

  it('retains neither an absent state nor a duration alone', () => {
    expect(nothingToShow(null)).toBe(true)
    expect(nothingToShow(state({ artist: null, title: null, album: null }))).toBe(true)
    // "3:34" without title nor artist informs nobody.
    expect(nothingToShow(state({ artist: null, title: null, album: null, duration_s: 214 }))).toBe(true)
  })
})

describe('usePlayer', () => {
  it('opens the pushed stream and applies each frame', () => {
    FakeEventSource.instances = []
    vi.stubGlobal('EventSource', FakeEventSource)
    const { state: current, ouvre, ferme } = usePlayer()
    ouvre()
    const stream = FakeEventSource.instances[0]!
    expect(stream.url).toBe('/api/player')
    expect(current.value).toBeNull()

    stream.push(state({ title: 'first' }))
    expect(current.value?.title).toBe('first')
    stream.push(state({ title: 'second' }))
    expect(current.value?.title).toBe('second')

    ferme()
    expect(stream.closed).toBe(true)
  })

  it('keeps the previous display on an unreadable frame', () => {
    // Emptying the screen because a frame is corrupt would be more misleading
    // than leaving the previous track one second too long.
    FakeEventSource.instances = []
    vi.stubGlobal('EventSource', FakeEventSource)
    const { state: current, ouvre } = usePlayer()
    ouvre()
    const stream = FakeEventSource.instances[0]!
    stream.push(state({ title: 'known' }))
    stream.push('not json')
    expect(current.value?.title).toBe('known')
  })

  it('closes the previous stream rather than accumulating them', () => {
    FakeEventSource.instances = []
    vi.stubGlobal('EventSource', FakeEventSource)
    const { ouvre } = usePlayer()
    ouvre()
    ouvre()
    expect(FakeEventSource.instances).toHaveLength(2)
    expect(FakeEventSource.instances[0]!.closed).toBe(true)
  })

  it('without EventSource, warns and lets the rest of the page live', () => {
    vi.stubGlobal('EventSource', undefined)
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { state: current, ouvre } = usePlayer()
    expect(() => ouvre()).not.toThrow()
    expect(current.value).toBeNull()
    expect(warn).toHaveBeenCalled()
    warn.mockRestore()
  })
})
