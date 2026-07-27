import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import { REMOTE_COMMANDS, REMOTE_POWER, REMOTE_ROWS } from './remoteCommands'

/** Faux `EventSource` : jsdom n'en fournit pas. */
class FauxEventSource {
  static derniere: FauxEventSource | null = null
  onmessage: ((e: MessageEvent) => void) | null = null
  constructor(public url: string) {
    FauxEventSource.derniere = this
  }
  close() {}
  pousse(etat: Partial<PlayerPayload>) {
    const complet: PlayerPayload = {
      source: 'radio',
      volume: 60,
      muted: false,
      standby: false,
      preset: null,
      artist: null,
      title: null,
      album: null,
      duration_s: null,
      origin: null,
      ...etat,
    }
    this.onmessage?.({ data: JSON.stringify(complet) } as MessageEvent)
  }
}

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

  it('met en évidence la touche de la présélection qui joue, et l’éteint à l’arrêt', async () => {
    // « On ne sait pas sur quel preset on est » : la touche correspondant à ce
    // qui joue (déclarée par la source active via le flux poussé) porte
    // aria-current et la variante pleine ; les autres restent neutres.
    vi.stubGlobal('EventSource', FauxEventSource)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ preset: 3 })
    await w.vm.$nextTick()
    const actif = w.find('[data-preset-button="3"]')
    expect(actif.attributes('data-preset-active')).toBe('true')
    expect(actif.attributes('aria-current')).toBe('true')
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
    // Plus rien ne joue : plus aucune touche en évidence.
    FauxEventSource.derniere!.pousse({ preset: null })
    await w.vm.$nextTick()
    expect(w.findAll('[data-preset-active]')).toHaveLength(0)
  })

  it('relaie l’état poussé à l’encart Lecteur', async () => {
    // L'unique connexion SSE de la page vit dans HomeView : l'encart doit
    // recevoir le même état en prop (c'était son propre flux auparavant).
    vi.stubGlobal('EventSource', FauxEventSource)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ volume: 45 })
    await w.vm.$nextTick()
    expect(w.find('[data-volume]').text()).toBe('45 %')
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
