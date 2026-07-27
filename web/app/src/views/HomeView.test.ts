import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { REMOTE_COMMANDS, REMOTE_POWER, REMOTE_ROWS } from './remoteCommands'

describe('REMOTE_COMMANDS', () => {
  it('couvre les 10 commandes simples du protocole, veille comprise', () => {
    expect(REMOTE_COMMANDS).toHaveLength(10)
    expect(REMOTE_COMMANDS.map((c) => c.cmd.cmd).sort()).toEqual(
      [
        'Eject', 'Mute', 'Next', 'PlayPause', 'Power',
        'Prev', 'SourceCycle', 'Stop', 'VolumeDown', 'VolumeUp',
      ].sort(),
    )
  })

  it('groupe les commandes par rangée, dans l’ordre voulu', () => {
    // L'ordre est une demande explicite du propriétaire : transport, contenu,
    // son, appareil. Le figer ici evite qu'un remaniement du gabarit le change
    // sans qu'on s'en apercoive.
    expect(REMOTE_ROWS.map((r) => r.map((c) => c.cmd.cmd))).toEqual([
      ['PlayPause', 'Stop'],
      ['Next', 'Prev'],
      ['VolumeUp', 'VolumeDown', 'Mute'],
      ['SourceCycle', 'Eject'],
    ])
  })

  it('la veille est à part, et n’apparaît pas dans les rangées', () => {
    expect(REMOTE_POWER.cmd.cmd).toBe('Power')
    expect(REMOTE_ROWS.flat().map((c) => c.cmd.cmd)).not.toContain('Power')
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

  it('rend une rangée par groupe et la veille dans l’en-tête', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    expect(w.findAll('[data-remote-row]')).toHaveLength(REMOTE_ROWS.length)
    expect(w.find('[data-remote-power]').exists()).toBe(true)
  })

  it('la veille est dans le slot d’action de l’en-tête, donc sur la ligne du titre', async () => {
    // Ce n'est pas cosmetique : `CardHeader` est une grille qui ne passe en deux
    // colonnes qu'en presence d'un enfant `data-slot="card-action"`. Sans lui, le
    // bouton tombe sur la deuxieme ligne, sous le titre — c'est exactement ce
    // qui s'etait produit, et aucune classe utilitaire ajoutee a la main ne le
    // corrigeait.
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    const action = w.find('[data-slot="card-action"]')
    expect(action.exists()).toBe(true)
    expect(action.find('[data-remote-power]').exists()).toBe(true)
  })

  it('le bouton de veille poste la commande Power', async () => {
    const spy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
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
