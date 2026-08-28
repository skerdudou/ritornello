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
  it('rend |◀ ▶ ▶| ■ dans cet order, sans Éjecter sur une source sans tiroir', () => {
    const w = mounted({})
    expect(w.findAll('[data-remote-command]').map((b) => b.attributes('data-remote-command')))
      .toEqual(['Prev', 'PlayPause', 'Next', 'Stop'])
  })

  it('Éjecter apparaît quand la source déclare un tiroir', () => {
    const w = mounted({ can_eject: true })
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })

  it('l’icône de lecture suit playback', async () => {
    const w = mounted({ playback: 'playing' })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('playing')
    await w.setProps({ state: state({}) })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('stopped')
  })

  it('poste la command du bouton', async () => {
    const w = mounted({})
    await w.get('[data-remote-command="Next"]').trigger('click')
    expect(w.emitted('command')).toEqual([[{ cmd: 'Next' }]])
  })

  it('en veille tout est grisé', () => {
    const w = mounted({ standby: true })
    for (const b of w.findAll('[data-remote-command]')) expect(b.attributes('disabled')).toBeDefined()
  })

  it('centre le trio de transport sans compter le groupe secondaire', () => {
    // **La régression visée est un décentrage.** Les cinq boutons étaient tous
    // enfants directs du `justify-center` : la largeur d'Arrêt (et d'Éjecter le
    // cas échéant) entrait dans le centrage, et Précédent/Lecture/Suivant
    // partaient vers la gauche. La rangée doit donc porter trois enfants — un
    // vide souple, le trio, puis le groupe secondaire — et non cinq boutons.
    const w = mounted({ can_eject: true })
    const rangee = w.get('[data-transport]').element
    const enfants = Array.from(rangee.children)
    expect(enfants).toHaveLength(3)
    const [vide, principal, secondaire] = enfants as [Element, Element, Element]
    // Le vide de gauche : même souplesse que le groupe de droite, c'est ce qui
    // met le trio au milieu. Décoratif, donc masqué aux lecteurs d'écran.
    expect(vide.getAttribute('aria-hidden')).toBe('true')
    expect(vide.className).toContain('flex-1')
    expect(secondaire.className).toContain('flex-1')
    // **À toutes les largeurs.** Une première version masquait le vide au-delà
    // de `md` et réalignait la rangée à gauche : sur PC le trio restait collé
    // au bord et Arrêt partait à l'autre bout — le défaut que le propriétaire a
    // signalé sur capture. Aucune des deux colonnes souples ne doit porter de
    // variante qui la retire ou la fige.
    for (const cote of [vide, secondaire]) {
      expect(cote.className).not.toMatch(/\bmd:hidden\b/)
      expect(cote.className).not.toMatch(/\bmd:flex-none\b/)
    }
    expect(
      Array.from(principal.querySelectorAll('[data-remote-command]')).map((b) =>
        b.getAttribute('data-remote-command'),
      ),
    ).toEqual(['Prev', 'PlayPause', 'Next'])
    expect(
      Array.from(secondaire.querySelectorAll('[data-remote-command]')).map((b) =>
        b.getAttribute('data-remote-command'),
      ),
    ).toEqual(['Stop', 'Eject'])
  })
})
