import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'
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
      preset_count: null,
      preset_name: null,
      status: null,
      overlay: null,
      artist: null,
      title: null,
      album: null,
      duration_s: null,
      origin: null,
      position_s: null,
      seekable: false,
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
    // son, appareil — et dans chaque rangée le sens du geste, « précédent »
    // avant « suivant » et « moins » avant « plus ». Le figer ici evite qu'un
    // remaniement du gabarit le change sans qu'on s'en apercoive.
    expect(REMOTE_ROWS.map((r) => r.map((c) => c.cmd.cmd))).toEqual([
      ['PlayPause', 'Stop'],
      ['Prev', 'Next'],
      ['VolumeDown', 'VolumeUp', 'Mute'],
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

  it('sans compte déclaré, la grille retombe sur 1-9', async () => {
    // `preset_count: null` (defaut de FauxEventSource, jamais poussee ici) :
    // la source ne declare rien, on garde la grille nue historique et pas de
    // +10 (rien a decaler vers).
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    expect(w.findAll('[data-preset-button]')).toHaveLength(9)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
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

  it('annonce le nombre de présélections déclaré par la source', async () => {
    vi.stubGlobal('EventSource', FauxEventSource)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ preset_count: 24 })
    await w.vm.$nextTick()
    // Le compte porte au-delà de la fenêtre affichée : c'est justement ce qu'il
    // apprend, la grille n'en montrant que neuf à la fois.
    expect(w.get('[data-preset-count]').text()).toContain('24')
    expect(w.findAll('[data-preset-button]')).toHaveLength(9)
    // Zéro est une information, pas une absence : il explique la grille vide
    // d'un cd sans disque.
    FauxEventSource.derniere!.pousse({ preset_count: 0 })
    await w.vm.$nextTick()
    expect(w.get('[data-preset-count]').text()).toContain('0')
  })

  it('n’annonce aucun compte quand la source ne déclare rien', async () => {
    // Grille nue 1-9 : c'est un repli, pas un inventaire — annoncer « 9 »
    // serait une affirmation que personne n'a faite.
    vi.stubGlobal('EventSource', FauxEventSource)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ preset_count: null })
    await w.vm.$nextTick()
    expect(w.find('[data-preset-count]').exists()).toBe(false)
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

describe('HomeView — volume maintenu', () => {
  /** Monte la vue avec des timings servis par /api/settings et des faux minuteurs. */
  async function monterAvecTimings(reglages = { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false }) {
    vi.useFakeTimers()
    const posts: string[] = []
    const spy = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push(String(init.body))
        return new Response(null, { status: 204 })
      }
      if (url === '/api/settings') return new Response(JSON.stringify(reglages), { status: 200 })
      return new Response('{}', { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    // Laisse le GET /api/settings du montage se résoudre sous faux minuteurs.
    await vi.runOnlyPendingTimersAsync()
    return { w, posts }
  }

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  it('un appui simple envoie une seule commande', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerdown')
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerup')
    await vi.advanceTimersByTimeAsync(5000)
    expect(posts).toEqual([JSON.stringify({ cmd: 'VolumeUp' })])
  })

  it('un appui maintenu répète après le délai initial puis à l’intervalle', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeDown"]').trigger('pointerdown')
    expect(posts).toHaveLength(1) // le pas immédiat
    await vi.advanceTimersByTimeAsync(999)
    expect(posts).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(posts).toHaveLength(2) // premier pas répété à 1000 ms
    await vi.advanceTimersByTimeAsync(500)
    expect(posts).toHaveLength(3)
    await vi.advanceTimersByTimeAsync(500)
    expect(posts).toHaveLength(4)
    await w.find('[data-remote-hold="VolumeDown"]').trigger('pointerup')
    await vi.advanceTimersByTimeAsync(5000)
    expect(posts).toHaveLength(4) // plus rien après le relâchement
  })

  it('les timings viennent de /api/settings', async () => {
    const { w, posts } = await monterAvecTimings({ volume_repeat_initial_ms: 200, volume_repeat_interval_ms: 100, start_in_standby: false })
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerdown')
    await vi.advanceTimersByTimeAsync(200)
    expect(posts).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(100)
    expect(posts).toHaveLength(3)
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerup')
  })

  it('quitter le bouton pendant le maintien arrête la répétition', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerdown')
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerleave')
    await vi.advanceTimersByTimeAsync(5000)
    expect(posts).toHaveLength(1)
  })

  it('l’auto-répétition du clavier ne mitraille pas : un seul pas par appui', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeUp"]').trigger('keydown.enter', { repeat: false })
    await w.find('[data-remote-hold="VolumeUp"]').trigger('keydown.enter', { repeat: true })
    expect(posts).toEqual([JSON.stringify({ cmd: 'VolumeUp' })])
  })
})

describe('HomeView — pagination des présélections', () => {
  /** Monte la vue et pousse un état poussé avec ces champs surchargés. */
  async function monterAvec(etat: Partial<PlayerPayload>) {
    vi.useFakeTimers()
    const posts: string[] = []
    const spy = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push(String(init.body))
        return new Response(null, { status: 204 })
      }
      return new Response('{}', { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse(etat)
    await nextTick()
    return { w, posts }
  }

  /** Numéros actuellement rendus, dans l'ordre — pas seulement leur nombre. */
  function numeros(w: Awaited<ReturnType<typeof monterAvec>>['w']) {
    return w.findAll('[data-preset-button]').map((b) => b.text())
  }

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  it('la grille ne montre que les numéros existants', async () => {
    const { w } = await monterAvec({ preset_count: 5 })
    expect(w.findAll('[data-preset-button]')).toHaveLength(5)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('un compte nul ne montre aucune touche numérotée', async () => {
    const { w } = await monterAvec({ preset_count: 0 })
    expect(w.findAll('[data-preset-button]')).toHaveLength(0)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('les deux flèches sont absentes avec 9 présélections ou moins', async () => {
    const { w } = await monterAvec({ preset_count: 9 })
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('> avance d’une page à travers toute la plage', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    expect(numeros(w)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9'])
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['10', '11', '12', '13', '14', '15', '16', '17', '18', '19'])
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['20', '21', '22', '23', '24'])
  })

  it('< revient à la page précédente', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    await w.find('[data-preset-next]').trigger('click')
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['20', '21', '22', '23', '24'])
    await w.find('[data-preset-prev]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['10', '11', '12', '13', '14', '15', '16', '17', '18', '19'])
  })

  it('bornes : < inactive sur la première page, > sur la dernière', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeUndefined()
    await w.find('[data-preset-next]').trigger('click')
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
  })

  it('aucun retour automatique après avoir avancé', async () => {
    // Ce test échouerait sans la suppression de la minuterie de retour.
    const { w } = await monterAvec({ preset_count: 23 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    vi.advanceTimersByTime(60_000)
    await nextTick()
    expect(numeros(w)).toEqual(['10', '11', '12', '13', '14', '15', '16', '17', '18', '19'])
  })

  it('un changement de compte ramène à la première page', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['10', '11', '12', '13', '14', '15', '16', '17', '18', '19'])
    // Nouvelle source, nouveau compte : la fenêtre ne doit pas survivre.
    FauxEventSource.derniere!.pousse({ preset_count: 5 })
    await nextTick()
    expect(numeros(w)).toEqual(['1', '2', '3', '4', '5'])
  })

  it('choisir une présélection laisse la page en place', async () => {
    const { w, posts } = await monterAvec({ preset_count: 23 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    await w.find('[data-preset-button="14"]').trigger('click')
    expect(posts).toEqual([JSON.stringify({ cmd: 'Select', arg: 14 })])
    expect(numeros(w)).toEqual(['10', '11', '12', '13', '14', '15', '16', '17', '18', '19'])
  })

  it('met en évidence la touche active au-delà de 9', async () => {
    const { w } = await monterAvec({ preset_count: 23, preset: 14 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(w.find('[data-preset-button="14"]').attributes('data-preset-active')).toBe('true')
  })
})
