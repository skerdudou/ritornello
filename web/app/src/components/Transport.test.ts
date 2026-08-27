import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import Transport from './Transport.vue'

const etat = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, ...e,
})
const monte = (e: Partial<PlayerPayload> | null) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Transport, { props: { etat: e ? etat(e) : null } })
}

describe('Transport', () => {
  it('rend |◀ ▶ ▶| ■ dans cet ordre, sans Éjecter sur une source sans tiroir', () => {
    const w = monte({})
    expect(w.findAll('[data-remote-command]').map((b) => b.attributes('data-remote-command')))
      .toEqual(['Prev', 'PlayPause', 'Next', 'Stop'])
  })

  it('Éjecter apparaît quand la source déclare un tiroir', () => {
    const w = monte({ can_eject: true })
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })

  it('l’icône de lecture suit playback', async () => {
    const w = monte({ playback: 'playing' })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('playing')
    await w.setProps({ etat: etat({}) })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('stopped')
  })

  it('poste la commande du bouton', async () => {
    const w = monte({})
    await w.get('[data-remote-command="Next"]').trigger('click')
    expect(w.emitted('commande')).toEqual([[{ cmd: 'Next' }]])
  })

  it('en veille tout est grisé', () => {
    const w = monte({ standby: true })
    for (const b of w.findAll('[data-remote-command]')) expect(b.attributes('disabled')).toBeDefined()
  })
})
