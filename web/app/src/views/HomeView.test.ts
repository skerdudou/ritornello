import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { REMOTE_COMMANDS } from './remoteCommands'

describe('REMOTE_COMMANDS', () => {
  it('couvre les 10 commandes simples du protocole', () => {
    expect(REMOTE_COMMANDS).toHaveLength(10)
    const cmds = REMOTE_COMMANDS.map((c) => c.cmd.cmd)
    expect(cmds).toEqual([
      'VolumeUp', 'VolumeDown', 'Mute', 'PlayPause', 'Stop',
      'Next', 'Prev', 'Eject', 'SourceCycle', 'Power',
    ])
  })

  it('chaque commande porte une clé de traduction', () => {
    for (const c of REMOTE_COMMANDS) expect(c.key).toMatch(/^remote_/)
  })
})

describe('HomeView', () => {
  it('poste la commande Select avec le numéro de présélection', async () => {
    const spy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', spy)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await w.find('[data-preset-button="3"]').trigger('click')
    expect(spy).toHaveBeenCalledWith(
      '/api/command',
      expect.objectContaining({ method: 'POST', body: JSON.stringify({ cmd: 'Select', arg: 3 }) }),
    )
  })

  it('expose les 9 présélections de la télécommande', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    expect(w.findAll('[data-preset-button]')).toHaveLength(9)
  })
})
