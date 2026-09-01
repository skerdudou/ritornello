import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import Transport from './Transport.vue'

const state = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, ...e,
})
const mounted = (e: Partial<PlayerPayload> | null) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Transport, { props: { state: e ? state(e) : null } })
}

describe('Transport', () => {
  it('renders |◀ ▶ ▶| ■ in that order, without Eject on a source without a tray', () => {
    const w = mounted({})
    expect(w.findAll('[data-remote-command]').map((b) => b.attributes('data-remote-command')))
      .toEqual(['Prev', 'PlayPause', 'Next', 'Stop'])
  })

  it('Eject appears when the source declares a tray', () => {
    const w = mounted({ can_eject: true })
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })

  it('the play icon follows playback', async () => {
    const w = mounted({ playback: 'playing' })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('playing')
    await w.setProps({ state: state({}) })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('stopped')
  })

  it('posts the button command', async () => {
    const w = mounted({})
    await w.get('[data-remote-command="Next"]').trigger('click')
    expect(w.emitted('command')).toEqual([[{ cmd: 'Next' }]])
  })

  it('in standby everything is greyed out', () => {
    const w = mounted({ standby: true })
    for (const b of w.findAll('[data-remote-command]')) expect(b.attributes('disabled')).toBeDefined()
  })

  it('centres the transport trio without counting the secondary group', () => {
    // **The targeted regression is an off-centring.** The five buttons were all
    // direct children of the `justify-center`: the width of Stop (and of Eject
    // when present) entered the centring, and Previous/Play/Next drifted to
    // the left. The row must therefore carry three children — a flexible void,
    // the trio, then the secondary group — and not five buttons.
    const w = mounted({ can_eject: true })
    const row = w.get('[data-transport]').element
    const children = Array.from(row.children)
    expect(children).toHaveLength(3)
    const [spacer, main, secondary] = children as [Element, Element, Element]
    // The void on the left: same flexibility as the group on the right, which
    // is what puts the trio in the middle. Decorative, hence hidden from
    // screen readers.
    expect(spacer.getAttribute('aria-hidden')).toBe('true')
    expect(spacer.className).toContain('flex-1')
    expect(secondary.className).toContain('flex-1')
    // **At every width.** A first version hid the void beyond `md` and
    // realigned the row to the left: on PC the trio stayed glued to the edge
    // and Stop went off to the other end — the defect the owner reported on a
    // screenshot. Neither of the two flexible columns must carry a variant
    // that removes or freezes it.
    for (const side of [spacer, secondary]) {
      expect(side.className).not.toMatch(/\bmd:hidden\b/)
      expect(side.className).not.toMatch(/\bmd:flex-none\b/)
    }
    expect(
      Array.from(main.querySelectorAll('[data-remote-command]')).map((b) =>
        b.getAttribute('data-remote-command'),
      ),
    ).toEqual(['Prev', 'PlayPause', 'Next'])
    expect(
      Array.from(secondary.querySelectorAll('[data-remote-command]')).map((b) =>
        b.getAttribute('data-remote-command'),
      ),
    ).toEqual(['Stop', 'Eject'])
  })
})
