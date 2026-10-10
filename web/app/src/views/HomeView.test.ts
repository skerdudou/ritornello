import { flushPromises, mount } from '@vue/test-utils'
import {
  afterEach, beforeAll, describe, expect, it, vi,
} from 'vitest'
import { nextTick } from 'vue'
import type { PlayerPayload } from '../types'
import {
  unavailable, hidden, REMOTE_COMMANDS, REMOTE_MUTE, REMOTE_POWER, REMOTE_SOURCE,
  REMOTE_TRANSPORT, REMOTE_TRANSPORT_SECONDARY,
} from './remoteCommands'
import { GAP, headerMode, KEY, STANDBY_BADGE } from './headerMode'

/** Fake `EventSource`: jsdom does not provide one. */
class FakeEventSource {
  static last: FakeEventSource | null = null
  onmessage: ((e: MessageEvent) => void) | null = null
  constructor(public url: string) {
    FakeEventSource.last = this
  }
  close() {}
  push(state: Partial<PlayerPayload>) {
    const full: PlayerPayload = {
      source: 'radio',
      volume: 60,
      muted: false,
      standby: false,
      preset: null,
      preset_count: null,
      preset_name: null,
      status: null,
      overlay: null,
      artist: null,
      title: null,
      album: null,
      duration_s: null,
      origin: null,
      cover_href: null,
      cover_origin: null,
      position_s: null,
      seekable: false,
      can_eject: false,
      has_finite_list: false,
      random: false,
      ...state,
    }
    this.onmessage?.({ data: JSON.stringify(full) } as MessageEvent)
  }
}

// HomeView always mounts the volume slider (reka-ui `Slider`): jsdom provides
// neither ResizeObserver (measuring the track on mount) nor the pointer
// capture methods it calls. Once for the whole file.
beforeAll(() => {
  Element.prototype.setPointerCapture ??= () => {}
  Element.prototype.releasePointerCapture ??= () => {}
  Element.prototype.hasPointerCapture ??= () => true
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
})

describe('REMOTE_COMMANDS', () => {
  it('covers the 10 commands of the page: the ±10 s and the step-by-step volume left the web', () => {
    // Decided during the redesign: seeking goes through the bar, the volume
    // through the slider. The four removed commands remain in the protocol
    // and on the physical remote. Random/repeat-all joined later, as
    // `SetRandom`/`SetRepeat`: the SPA knows the current value, so it
    // sends the absolute form, not the toggle reserved for a key that
    // doesn't (see `unavailable`'s doc).
    expect(REMOTE_COMMANDS).toHaveLength(10)
    expect(REMOTE_COMMANDS.map((c) => c.cmd.cmd).sort()).toEqual(
      ['Eject', 'Mute', 'Next', 'PlayPause', 'Power', 'Prev', 'SetRandom', 'SetRepeat', 'SourceCycle', 'Stop'].sort(),
    )
  })

  it('the transport goes in the direction of the gesture, play in the middle', () => {
    // |◀ ▶ ▶|: previous/next adjacent to play, that is the order of hi-fi
    // remotes; Stop and Eject behind.
    expect(REMOTE_TRANSPORT.map((c) => c.cmd.cmd)).toEqual(['Prev', 'PlayPause', 'Next'])
    expect(REMOTE_TRANSPORT_SECONDARY.map((c) => c.cmd.cmd)).toEqual(['Stop', 'Eject'])
  })

  it('standby, source and mute are set apart', () => {
    expect(REMOTE_POWER.cmd.cmd).toBe('Power')
    expect(REMOTE_SOURCE.cmd.cmd).toBe('SourceCycle')
    expect(REMOTE_MUTE.cmd.cmd).toBe('Mute')
    const transport = [...REMOTE_TRANSPORT, ...REMOTE_TRANSPORT_SECONDARY].map((c) => c.cmd.cmd)
    expect(transport).not.toContain('Power')
    expect(transport).not.toContain('SourceCycle')
    expect(transport).not.toContain('Mute')
  })

  it('every command carries a translation key', () => {
    for (const c of REMOTE_COMMANDS) expect(c.key).toMatch(/^remote_/)
  })
})

describe('HomeView', () => {
  it('posts the Select command with the preset number', async () => {
    const spy = vi.fn().mockImplementation(async () => new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', spy)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await w.find('[data-preset-button="3"]').trigger('click')
    expect(spy).toHaveBeenCalledWith(
      '/api/command',
      expect.objectContaining({ method: 'POST', body: JSON.stringify({ cmd: 'Select', arg: 3 }) }),
    )
  })

  it('without a declared count, the grid falls back on 1-10', async () => {
    // `preset_count: null` (FakeEventSource default, never pushed here): the
    // source declares nothing, we keep the historical bare grid and no +10
    // (nothing to shift towards).
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('standby is in the header action slot, hence on the title line', async () => {
    // This is not cosmetic: `CardHeader` is a grid that only switches to two
    // columns in the presence of a `data-slot="card-action"` child. Without
    // it, the button falls on the second line, under the title — exactly what
    // had happened, and no hand-added utility class fixed it.
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    const action = w.find('[data-slot="card-action"]')
    expect(action.exists()).toBe(true)
    expect(action.find('[data-remote-power]').exists()).toBe(true)
  })

  it('highlights the key of the playing preset, and turns it off on stop', async () => {
    // "We don't know which preset we are on": the key matching what plays
    // (declared by the active source through the pushed stream) carries
    // aria-current and the filled variant; the others stay neutral.
    vi.stubGlobal('EventSource', FakeEventSource)
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push({ preset: 3 })
    await w.vm.$nextTick()
    const active = w.find('[data-preset-button="3"]')
    expect(active.attributes('data-preset-active')).toBe('true')
    expect(active.attributes('aria-current')).toBe('true')
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
    // Nothing plays any more: no key highlighted any more.
    FakeEventSource.last!.push({ preset: null })
    await w.vm.$nextTick()
    expect(w.findAll('[data-preset-active]')).toHaveLength(0)
  })

  it('the announced count shapes the grid, with no lone number', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push({ preset_count: 24 })
    await w.vm.$nextTick()
    // The count reaches beyond the displayed window: the grid shows ten at a
    // time and the pages sit in the card's header. No lone number any more.
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-count]').exists()).toBe(false)
    expect(w.find('[data-preset-window]').exists()).toBe(true)
    // Zero is information, not an absence: it explains the empty grid of a
    // cd without a disc.
    FakeEventSource.last!.push({ preset_count: 0 })
    await w.vm.$nextTick()
    expect(w.findAll('[data-preset-button]')).toHaveLength(0)
  })

  it('shows no position when the source declares nothing', async () => {
    // Bare grid 1-10: it is a fallback, not an inventory — announcing "10"
    // would be a claim nobody made.
    vi.stubGlobal('EventSource', FakeEventSource)
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push({ preset_count: null })
    await w.vm.$nextTick()
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-window]').exists()).toBe(false)
  })

  it('relays the pushed state to the Player card', async () => {
    // The page's single SSE connection lives in HomeView: the card must
    // receive the same state as a prop (it used to be its own stream).
    vi.stubGlobal('EventSource', FakeEventSource)
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push({ volume: 45 })
    await w.vm.$nextTick()
    expect(w.find('[data-volume]').text()).toBe('45 %')
  })

  it('the standby button posts the Power command', async () => {
    const spy = vi.fn().mockImplementation(async () => new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', spy)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await w.find('[data-remote-power]').trigger('click')
    expect(spy).toHaveBeenCalledWith(
      '/api/command',
      expect.objectContaining({ method: 'POST', body: JSON.stringify({ cmd: 'Power' }) }),
    )
  })
})

describe('HomeView — preset pagination', () => {
  /** Mounts the view and pushes a state with these fields overridden. */
  async function mountWith(state: Partial<PlayerPayload>) {
    vi.useFakeTimers()
    const posts: string[] = []
    const spy = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push(String(init.body))
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push(state)
    await nextTick()
    return { w, posts }
  }

  /** Numbers currently rendered, in order — not only how many. */
  function numbers(w: Awaited<ReturnType<typeof mountWith>>['w']) {
    return w.findAll('[data-preset-button]').map((b) => b.text())
  }

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  it('the grid only shows the existing numbers', async () => {
    const { w } = await mountWith({ preset_count: 5 })
    expect(w.findAll('[data-preset-button]')).toHaveLength(5)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('a zero count shows no numbered key', async () => {
    const { w } = await mountWith({ preset_count: 0 })
    expect(w.findAll('[data-preset-button]')).toHaveLength(0)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('both arrows are absent with 10 presets or fewer', async () => {
    const { w } = await mountWith({ preset_count: 10 })
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('> advances one page through the whole range', async () => {
    const { w } = await mountWith({ preset_count: 24 })
    expect(numbers(w)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numbers(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numbers(w)).toEqual(['21', '22', '23', '24'])
  })

  it('< goes back to the previous page', async () => {
    const { w } = await mountWith({ preset_count: 24 })
    await w.find('[data-preset-next]').trigger('click')
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numbers(w)).toEqual(['21', '22', '23', '24'])
    await w.find('[data-preset-prev]').trigger('click')
    await nextTick()
    expect(numbers(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
  })

  it('bounds: < inactive on the first page, > on the last', async () => {
    const { w } = await mountWith({ preset_count: 24 })
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeUndefined()
    await w.find('[data-preset-next]').trigger('click')
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
  })

  it('no automatic return after advancing', async () => {
    // This test would fail without the removal of the return timer.
    const { w } = await mountWith({ preset_count: 23 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    vi.advanceTimersByTime(60_000)
    await nextTick()
    expect(numbers(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
  })

  it('a count change brings back to the first page', async () => {
    const { w } = await mountWith({ preset_count: 24 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numbers(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
    // New source, new count: the window must not survive.
    FakeEventSource.last!.push({ preset_count: 5 })
    await nextTick()
    expect(numbers(w)).toEqual(['1', '2', '3', '4', '5'])
  })

  it('choosing a preset leaves the page in place', async () => {
    const { w, posts } = await mountWith({ preset_count: 23 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    await w.find('[data-preset-button="14"]').trigger('click')
    expect(posts).toEqual([JSON.stringify({ cmd: 'Select', arg: 14 })])
    expect(numbers(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
  })

  it('highlights the active key beyond 9', async () => {
    // No more `>` to click here: the page already opens on the one containing
    // the playing number (see the block "the page follows what plays").
    const { w } = await mountWith({ preset_count: 23, preset: 14 })
    expect(w.find('[data-preset-button="14"]').attributes('data-preset-active')).toBe('true')
  })
})

describe('HomeView — the page follows what plays', () => {
  /** Mounts the view, pushes a first state, and returns what it takes to push others. */
  async function mountWith(state: Partial<PlayerPayload>) {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push(state)
    await nextTick()
    return w
  }

  function numbers(w: Awaited<ReturnType<typeof mountWith>>) {
    return w.findAll('[data-preset-button]').map((b) => b.text())
  }

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('opens on the page of the playing preset', async () => {
    // The case that motivated everything: arriving on the tab while station 24
    // plays showed 1-10, with no key highlighted.
    const w = await mountWith({ preset_count: 40, preset: 24 })
    expect(numbers(w)).toEqual(['21', '22', '23', '24', '25', '26', '27', '28', '29', '30'])
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
  })

  it('follows a page change coming from elsewhere (infrared remote, +10)', async () => {
    const w = await mountWith({ preset_count: 40, preset: 3 })
    expect(numbers(w)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    FakeEventSource.last!.push({ preset_count: 40, preset: 31 })
    await nextTick()
    expect(numbers(w)).toEqual(['31', '32', '33', '34', '35', '36', '37', '38', '39', '40'])
  })

  it('10 and 11 are on either side of the boundary', async () => {
    // The grid's bounds are those of the core's offset: a page covers
    // 10k+1..10k+10, so 10 closes page 0 and 11 opens page 1. The boundary was
    // at 9/10 when the remote's key 0 was worth nothing on its own.
    const w = await mountWith({ preset_count: 40, preset: 10 })
    expect(numbers(w)[0]).toBe('1')
    FakeEventSource.last!.push({ preset_count: 40, preset: 11 })
    await nextTick()
    expect(numbers(w)[0]).toBe('11')
  })

  it('a stop leaves the page where it is', async () => {
    // `preset` falls back to null without the count moving: nothing justifies
    // sending the user back to the first page, they are still looking at this
    // group.
    const w = await mountWith({ preset_count: 40, preset: 24 })
    FakeEventSource.last!.push({ preset_count: 40, preset: null })
    await nextTick()
    expect(numbers(w)[0]).toBe('21')
  })

  it('a manual pagination survives frames that change nothing', async () => {
    const w = await mountWith({ preset_count: 40, preset: 3 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numbers(w)[0]).toBe('11')
    // Same preset, same count, only the volume changes: the page stays.
    FakeEventSource.last!.push({ preset_count: 40, preset: 3, volume: 42 })
    await nextTick()
    expect(numbers(w)[0]).toBe('11')
  })

  it('a number beyond the count does not open an empty page', async () => {
    // Inconsistent source (the count shrank before the preset followed): we
    // clamp on the last non-empty page rather than showing no key.
    const w = await mountWith({ preset_count: 12, preset: 35 })
    expect(numbers(w)).toEqual(['11', '12'])
  })
})

describe('HomeView — unavailable buttons', () => {
  async function mountWith(state: Partial<PlayerPayload>) {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push(state)
    await nextTick()
    return w
  }

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('in standby, only standby and the source choice stay offered', async () => {
    // The core ignores everything but `Power`, `SelectSource` and
    // `SourceCycle` in standby: choosing a source wakes the device on it. The
    // buttons say so instead of sending a command with no effect.
    const w = await mountWith({ standby: true })
    for (const b of w.findAll('[data-remote-command]')) {
      expect(b.attributes('disabled'), b.attributes('data-remote-command')).toBeDefined()
    }
    for (const b of w.findAll('[data-preset-button]')) {
      expect(b.attributes('disabled')).toBeDefined()
    }
    expect(w.get('[data-remote-source]').attributes('disabled')).toBeUndefined()
    expect(w.get('[data-remote-power]').attributes('disabled')).toBeUndefined()
  })

  it('the Mute key shows its state, where one acts on the volume', async () => {
    // Requested from use: muted sound was read in the card, at the top of the
    // page, whereas it is muted from this row — at the bottom. The key thus
    // carries its own state, in addition to the card's mention.
    const muted = await mountWith({ standby: false, muted: true })
    const key = muted.get('[data-remote-command="Mute"]')
    expect(key.attributes('data-on')).toBe('true')
    expect(key.attributes('aria-pressed')).toBe('true')

    const audible = await mountWith({ standby: false, muted: false })
    const rendered = audible.get('[data-remote-command="Mute"]')
    expect(rendered.attributes('data-on')).toBeUndefined()
    expect(rendered.attributes('aria-pressed')).toBe('false')
  })

  it('Eject is hidden on the radio and present on the cd player, disc or not', async () => {
    // The source declares it itself — the page never compares `source` to
    // `'cd'`, that name coming from plugins.toml. Hidden rather than greyed
    // (see `hidden`): the radio has no tray, a cd without a disc has one.
    const w = await mountWith({ source: 'radio', can_eject: false })
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(false)
    FakeEventSource.last!.push({ can_eject: true, preset_count: 0, status: 'NO DISC' })
    await nextTick()
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })

  it('before the first frame, nothing is greyed', async () => {
    // The remote opens usable: greying blindly, then un-greying, would make
    // the whole card flicker on every tab opening.
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    for (const b of w.findAll('[data-remote-command]')) {
      expect(b.attributes('disabled')).toBeUndefined()
    }
    expect(w.get('[data-preset-button="1"]').attributes('disabled')).toBeUndefined()
  })
})

describe('HomeView — sliders and names', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('the volume slider posts SetVolume, absolute', async () => {
    const posts: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') { posts.push(String(init.body)); return new Response(null, { status: 204 }) }
      if (url === '/api/presets') return new Response(JSON.stringify({ sources: [] }), { status: 200 })
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push({ volume: 60 })
    await nextTick()
    const thumb = w.get('[data-volume-slider] [role="slider"]')
    ;(thumb.element as HTMLElement).focus()
    await thumb.trigger('keydown', { key: 'ArrowRight' })
    expect(posts).toContain(JSON.stringify({ cmd: 'SetVolume', arg: 61 }))
  })

  it('the bar posts SeekTo, by the step configured by /api/settings', async () => {
    const posts: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') { posts.push(String(init.body)); return new Response(null, { status: 204 }) }
      if (url === '/api/presets') return new Response(JSON.stringify({ sources: [] }), { status: 200 })
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FakeEventSource.last!.push({ seekable: true, duration_s: 254, position_s: 87 })
    await flushPromises()
    const thumb = w.get('[data-bar] [role="slider"]')
    ;(thumb.element as HTMLElement).focus()
    await thumb.trigger('keydown', { key: 'ArrowRight' })
    // The step comes from /api/settings (`seek_step_s: 10`, stubbed above):
    // 87 + 10 = 97.
    expect(posts).toContain(JSON.stringify({ cmd: 'SeekTo', arg: 97 }))
  })

  it('names the tiles from /api/presets and reloads on source change', async () => {
    const gets: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return new Response(null, { status: 204 })
      gets.push(url)
      if (url === '/api/presets') {
        return new Response(JSON.stringify({ sources: [
          { name: 'radio', presets: [{ index: 1, name: 'FIP' }] },
          { name: 'files', presets: [{ index: 1, name: 'tout.m3u' }] },
        ] }), { status: 200 })
      }
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    vi.stubGlobal('EventSource', FakeEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await flushPromises()
    FakeEventSource.last!.push({ source: 'radio', preset_count: 3 })
    await nextTick()
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    const before = gets.filter((u) => u === '/api/presets').length
    FakeEventSource.last!.push({ source: 'files', preset_count: 3 })
    await flushPromises()
    expect(gets.filter((u) => u === '/api/presets').length).toBe(before + 1)
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('tout.m3u')
  })
})

describe('headerMode', () => {
  // Three keys, the gaps between them, the gap before standby, standby.
  const NEEDED = 3 * KEY + 2 * GAP + GAP + KEY

  it('the measures are those of the kit: icon-sm is size-8, the row is gap-1', () => {
    expect(KEY).toBe(32)
    expect(GAP).toBe(4)
  })

  it('a single source, once the list is read: nothing to choose between', () => {
    expect(headerMode({ sources: 1, listRead: true, available: 1000, standby: false, activeListed: true })).toBe('none')
    expect(headerMode({ sources: 0, listRead: true, available: 1000, standby: false, activeListed: true })).toBe('none')
  })

  it('an unread list keeps the cycle key: a failed /api/presets must leave a way to change source', () => {
    expect(headerMode({ sources: 0, listRead: false, available: 1000, standby: false, activeListed: true })).toBe('cycle')
    expect(headerMode({ sources: 3, listRead: false, available: 1000, standby: false, activeListed: true })).toBe('cycle')
  })

  it('an unknown width keeps the cycle key', () => {
    expect(headerMode({ sources: 3, listRead: true, available: null, standby: false, activeListed: true })).toBe('cycle')
  })

  it('the icons from exactly the width they need, the cycle key one pixel below', () => {
    expect(NEEDED).toBe(140)
    expect(headerMode({ sources: 3, listRead: true, available: NEEDED, standby: false, activeListed: true })).toBe('icons')
    expect(headerMode({ sources: 3, listRead: true, available: NEEDED - 1, standby: false, activeListed: true })).toBe('cycle')
  })

  it('standby makes room for its badge', () => {
    const withBadge = NEEDED + STANDBY_BADGE + GAP
    expect(headerMode({ sources: 3, listRead: true, available: withBadge, standby: false, activeListed: true })).toBe('icons')
    expect(headerMode({ sources: 3, listRead: true, available: withBadge, standby: true, activeListed: true })).toBe('icons')
    expect(headerMode({ sources: 3, listRead: true, available: withBadge - 1, standby: true, activeListed: true })).toBe('cycle')
    // The same width that holds the keys alone no longer does with the badge.
    expect(headerMode({ sources: 3, listRead: true, available: NEEDED, standby: true, activeListed: true })).toBe('cycle')
  })

  it('an active source absent from the list is named by the pill, even with ample width', () => {
    // The core keeps the active source while mpv plays after its plugin died.
    expect(headerMode({ sources: 3, listRead: true, available: 1000, standby: false, activeListed: false })).toBe('cycle')
    // Listed (or empty, which the caller reports as listed): unchanged.
    expect(headerMode({ sources: 3, listRead: true, available: 1000, standby: false, activeListed: true })).toBe('icons')
  })
})

describe('HomeView — the source keys in the Player card header', () => {
  /**
   * A `ResizeObserver` that reports `width` for the Player card's header, and
   * nothing for anything else (the volume slider observes its own track).
   * Without it the width stays unknown, which is the cycle key's case.
   */
  function headerWidth(width: number) {
    return class {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe(target: Element) {
        if (!target.matches('[data-player-header]')) return
        const entry = { target, contentRect: { width } } as unknown as ResizeObserverEntry
        this.callback([entry], this as unknown as ResizeObserver)
      }
      unobserve() {}
      disconnect() {}
    }
  }

  async function mountWith(sources: unknown, state: Partial<PlayerPayload> = {}, width: number | null = WIDE) {
    const posts: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push(init.body as string)
        return new Response(null, { status: 204 })
      }
      if (url === '/api/presets') return new Response(JSON.stringify(sources), { status: 200 })
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    vi.stubGlobal('EventSource', FakeEventSource)
    if (width !== null) vi.stubGlobal('ResizeObserver', headerWidth(width))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await flushPromises()
    FakeEventSource.last!.push(state)
    await nextTick()
    return { w, posts }
  }

  const WIDE = 1000
  const NARROW = 100
  const THREE = { sources: [{ name: 'radio', icon: 'radio' }, { name: 'cd', icon: 'disc' }, { name: 'files' }] }

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('wide: one icon key per declared source, in the order of the cycle', async () => {
    const { w } = await mountWith(THREE, { source: 'cd' })
    const keys = w.findAll('[data-source-key]')
    expect(keys.map((k) => k.attributes('data-source-key'))).toEqual(['radio', 'cd', 'files'])
    // The announced icon, or the initial of a source that announced none.
    expect(w.find('[data-source-key="radio"] [data-source-icon="radio"]').exists()).toBe(true)
    expect(w.find('[data-source-key="cd"] [data-source-icon="disc"]').exists()).toBe(true)
    expect(w.get('[data-source-key="files"] [data-source-initial]').text()).toBe('F')
  })

  it('wide: every key carries its own name, the icon being hidden from assistive technology', async () => {
    const { w } = await mountWith(THREE, { source: 'cd' })
    for (const k of w.findAll('[data-source-key]')) {
      const name = k.attributes('data-source-key')
      expect(k.attributes('aria-label')).toBe(name)
      expect(k.attributes('title')).toBe(name)
    }
  })

  it('wide: the active source is the pressed key, and only it', async () => {
    const { w } = await mountWith(THREE, { source: 'cd' })
    const pressed = w.findAll('[data-source-key]').map((k) => k.attributes('aria-pressed'))
    expect(pressed).toEqual(['false', 'true', 'false'])
  })

  it('wide: the playing dot sits on the active key while it plays, and only then', async () => {
    const { w } = await mountWith(THREE, { source: 'cd', playback: 'playing' })
    expect(w.findAll('[data-now-playing-line]')).toHaveLength(1)
    expect(w.get('[data-source-key="cd"]').find('[data-now-playing-line]').exists()).toBe(true)
    FakeEventSource.last!.push({ source: 'cd', playback: 'paused' })
    await nextTick()
    expect(w.find('[data-now-playing-line]').exists()).toBe(false)
  })

  it('wide: no cycle key, no source pill, no visible title; the card keeps its name', async () => {
    const { w } = await mountWith(THREE, { source: 'cd' })
    expect(w.find('[data-remote-source]').exists()).toBe(false)
    expect(w.find('[data-source]').exists()).toBe(false)
    const card = w.get('[data-player]')
    const name = card.attributes('aria-label')
    expect(name).toBeTruthy()
    expect(w.get('[data-player-header]').text()).not.toContain(name)
  })

  it('wide: a key posts SelectSource with the name, not a notch of the cycle', async () => {
    const { w, posts } = await mountWith(THREE, { source: 'radio' })
    await w.get('[data-source-key="files"]').trigger('click')
    expect(posts).toEqual([JSON.stringify({ cmd: 'SelectSource', arg: 'files' })])
  })

  it('wide: in standby, every key stays offered, like the cycle key', async () => {
    // Choosing a source wakes the device on it (see `unavailable`).
    const { w } = await mountWith(THREE, { standby: true })
    const keys = w.findAll('[data-source-key]')
    expect(keys).toHaveLength(3)
    for (const k of keys) {
      expect(k.attributes('disabled'), k.attributes('data-source-key')).toBeUndefined()
    }
  })

  it('narrow: the cycle key and the source pill, no icon key', async () => {
    const { w } = await mountWith(THREE, { source: 'cd' }, NARROW)
    expect(w.find('[data-remote-source]').exists()).toBe(true)
    expect(w.get('[data-source]').text()).toBe('cd')
    expect(w.find('[data-source-key]').exists()).toBe(false)
  })

  it('wide, but the active source is not in the list: the pill names it', async () => {
    // The core keeps the active source while mpv plays after its plugin died.
    const { w } = await mountWith(THREE, { source: 'upnp' })
    expect(w.find('[data-source-key]').exists()).toBe(false)
    expect(w.find('[data-remote-source]').exists()).toBe(true)
    expect(w.get('[data-source]').text()).toBe('upnp')
  })

  it('a width not measured yet keeps the cycle key', async () => {
    const { w } = await mountWith(THREE, { source: 'cd' }, null)
    expect(w.find('[data-remote-source]').exists()).toBe(true)
    expect(w.find('[data-source-key]').exists()).toBe(false)
  })

  it('without a catalog, the cycle key stays, whatever the width', async () => {
    // A failed /api/presets must not leave a wide screen with no way at all
    // to change source.
    const { w } = await mountWith({ seek_step_s: 10 })
    expect(w.find('[data-remote-source]').exists()).toBe(true)
    expect(w.find('[data-source-key]').exists()).toBe(false)
  })

  it('a single source: neither keys nor cycle key, only the pill naming it', async () => {
    const { w } = await mountWith({ sources: [{ name: 'radio' }] }, { source: 'radio' })
    expect(w.find('[data-source-key]').exists()).toBe(false)
    expect(w.find('[data-remote-source]').exists()).toBe(false)
    expect(w.get('[data-source]').text()).toBe('radio')
  })

  it('the bar above the cards is gone, at every width', async () => {
    for (const width of [WIDE, NARROW]) {
      const { w } = await mountWith(THREE, { source: 'cd' }, width)
      expect(w.find('[data-source-bar]').exists()).toBe(false)
    }
  })
})

describe('unavailable / hidden', () => {
  const state = (e: Partial<PlayerPayload>): PlayerPayload => ({
    source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
    preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
    duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
    seekable: false, can_eject: false, has_finite_list: false, random: false, ...e,
  })

  it('in standby, SelectSource and SourceCycle stay available', () => {
    const s = state({ standby: true })
    expect(unavailable('SelectSource', s)).toBe(false)
    expect(unavailable('SourceCycle', s)).toBe(false)
    expect(unavailable('Power', s)).toBe(false)
    expect(unavailable('Select', s)).toBe(true)
    expect(unavailable('Next', s)).toBe(true)
  })

  it('standby lets only Power and the source choice through', () => {
    expect(unavailable('Power', state({ standby: true }))).toBe(false)
    expect(unavailable('PlayPause', state({ standby: true }))).toBe(true)
    expect(unavailable('Select', state({ standby: true }))).toBe(true)
  })

  it('out of standby, nothing is greyed: seeking no longer has a key, eject hides itself', () => {
    expect(unavailable('PlayPause', state({}))).toBe(false)
    expect(unavailable('Eject', state({ can_eject: false }))).toBe(false)
  })

  it('the two mode buttons grey on the source\'s capability, not on the mode\'s own value', () => {
    // `has_finite_list` is what decides it — a capability of the active
    // source, like `can_eject` — never `random`/`repeat` themselves:
    // those are persisted settings (see their doc in the proto crate) that
    // must not be read as a proxy for the capability.
    expect(unavailable('SetRandom', state({ has_finite_list: false }))).toBe(true)
    expect(unavailable('SetRepeat', state({ has_finite_list: false }))).toBe(true)
    expect(unavailable('SetRandom', state({ has_finite_list: true }))).toBe(false)
    expect(unavailable('SetRepeat', state({ has_finite_list: true }))).toBe(false)
    // The mutation this guards against: a rule reading `random`/`repeat`
    // themselves instead of `has_finite_list` would still pass the two lines
    // above (both default to `false`) but would fail here.
    expect(unavailable('SetRandom', state({ has_finite_list: true, random: true }))).toBe(false)
    expect(unavailable('SetRepeat', state({ has_finite_list: true, repeat: 'all' }))).toBe(false)
  })

  it('Eject is hidden as long as the source declares no tray, including before the first frame', () => {
    // `can_eject` is a capability the plugin declares for itself (the cd
    // declares it disc or not): hiding it never hides a player that exists.
    // Before the first frame, we do not know — so nothing.
    expect(hidden('Eject', null)).toBe(true)
    expect(hidden('Eject', state({ can_eject: false }))).toBe(true)
    expect(hidden('Eject', state({ can_eject: true }))).toBe(false)
    expect(hidden('Stop', state({ can_eject: false }))).toBe(false)
  })

  it('an unknown state greys nothing', () => {
    expect(unavailable('PlayPause', null)).toBe(false)
  })
})
