import { flushPromises, mount } from '@vue/test-utils'
import {
  afterEach, beforeAll, describe, expect, it, vi,
} from 'vitest'
import { nextTick } from 'vue'
import type { PlayerPayload } from '../types'
import {
  indisponible, masquee, REMOTE_COMMANDS, REMOTE_MUTE, REMOTE_POWER, REMOTE_SOURCE,
  REMOTE_TRANSPORT, REMOTE_TRANSPORT_SECONDAIRE,
} from './remoteCommands'

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
      cover_href: null,
      cover_origin: null,
      position_s: null,
      seekable: false,
      can_eject: false,
      ...etat,
    }
    this.onmessage?.({ data: JSON.stringify(complet) } as MessageEvent)
  }
}

// HomeView monte toujours le curseur de volume (reka-ui `Slider`) : jsdom ne
// fournit ni ResizeObserver (mesure de la piste au montage) ni les méthodes
// de capture de pointeur qu'il appelle. Une fois pour tout le fichier.
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
  it('couvre les 8 commandes de la page : les ±10 s et le volume pas à pas ont quitté le web', () => {
    // Décidé au chantier refonte : le déplacement passe par la barre, le
    // volume par le curseur. Les quatre commandes retirées restent dans le
    // protocole et sur la télécommande physique.
    expect(REMOTE_COMMANDS).toHaveLength(8)
    expect(REMOTE_COMMANDS.map((c) => c.cmd.cmd).sort()).toEqual(
      ['Eject', 'Mute', 'Next', 'PlayPause', 'Power', 'Prev', 'SourceCycle', 'Stop'].sort(),
    )
  })

  it('le transport va dans le sens du geste, la lecture au centre', () => {
    // |◀ ▶ ▶| : précédent/suivant adjacents à lecture, c'est l'ordre des
    // télécommandes hi-fi ; Stop et Éjecter en retrait.
    expect(REMOTE_TRANSPORT.map((c) => c.cmd.cmd)).toEqual(['Prev', 'PlayPause', 'Next'])
    expect(REMOTE_TRANSPORT_SECONDAIRE.map((c) => c.cmd.cmd)).toEqual(['Stop', 'Eject'])
  })

  it('la veille, la source et le muet sont à part', () => {
    expect(REMOTE_POWER.cmd.cmd).toBe('Power')
    expect(REMOTE_SOURCE.cmd.cmd).toBe('SourceCycle')
    expect(REMOTE_MUTE.cmd.cmd).toBe('Mute')
    const transport = [...REMOTE_TRANSPORT, ...REMOTE_TRANSPORT_SECONDAIRE].map((c) => c.cmd.cmd)
    expect(transport).not.toContain('Power')
    expect(transport).not.toContain('SourceCycle')
    expect(transport).not.toContain('Mute')
  })

  it('chaque commande porte une clé de traduction', () => {
    for (const c of REMOTE_COMMANDS) expect(c.key).toMatch(/^remote_/)
  })
})

describe('HomeView', () => {
  it('poste la commande Select avec le numéro de présélection', async () => {
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

  it('sans compte déclaré, la grille retombe sur 1-10', async () => {
    // `preset_count: null` (defaut de FauxEventSource, jamais poussee ici) :
    // la source ne declare rien, on garde la grille nue historique et pas de
    // +10 (rien a decaler vers).
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('la veille est dans le slot d’action de l’en-tête, donc sur la ligne du titre', async () => {
    // Ce n'est pas cosmetique : `CardHeader` est une grille qui ne passe en deux
    // colonnes qu'en presence d'un enfant `data-slot="card-action"`. Sans lui, le
    // bouton tombe sur la deuxieme ligne, sous le titre — c'est exactement ce
    // qui s'etait produit, et aucune classe utilitaire ajoutee a la main ne le
    // corrigeait.
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
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
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
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
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ preset_count: 24 })
    await w.vm.$nextTick()
    // Le compte porte au-delà de la fenêtre affichée : c'est justement ce qu'il
    // apprend, la grille n'en montrant que dix à la fois.
    expect(w.get('[data-preset-count]').text()).toContain('24')
    expect(w.findAll('[data-preset-button]')).toHaveLength(10)
    // Zéro est une information, pas une absence : il explique la grille vide
    // d'un cd sans disque.
    FauxEventSource.derniere!.pousse({ preset_count: 0 })
    await w.vm.$nextTick()
    expect(w.get('[data-preset-count]').text()).toContain('0')
  })

  it('n’annonce aucun compte quand la source ne déclare rien', async () => {
    // Grille nue 1-10 : c'est un repli, pas un inventaire — annoncer « 10 »
    // serait une affirmation que personne n'a faite.
    vi.stubGlobal('EventSource', FauxEventSource)
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
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
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ volume: 45 })
    await w.vm.$nextTick()
    expect(w.find('[data-volume]').text()).toBe('45 %')
  })

  it('le bouton de veille poste la commande Power', async () => {
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
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
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

  it('les deux flèches sont absentes avec 10 présélections ou moins', async () => {
    const { w } = await monterAvec({ preset_count: 10 })
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-next]').exists()).toBe(false)
  })

  it('> avance d’une page à travers toute la plage', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    expect(numeros(w)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['21', '22', '23', '24'])
  })

  it('< revient à la page précédente', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    await w.find('[data-preset-next]').trigger('click')
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['21', '22', '23', '24'])
    await w.find('[data-preset-prev]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
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
    expect(numeros(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
  })

  it('un changement de compte ramène à la première page', async () => {
    const { w } = await monterAvec({ preset_count: 24 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
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
    expect(numeros(w)).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
  })

  it('met en évidence la touche active au-delà de 9', async () => {
    // Plus de `>` à cliquer ici : la page s'ouvre déjà sur celle qui contient
    // le numéro qui joue (voir le bloc « la page suit ce qui joue »).
    const { w } = await monterAvec({ preset_count: 23, preset: 14 })
    expect(w.find('[data-preset-button="14"]').attributes('data-preset-active')).toBe('true')
  })
})

describe('HomeView — la page suit ce qui joue', () => {
  /** Monte la vue, pousse un premier état, et rend de quoi en pousser d'autres. */
  async function monterAvec(etat: Partial<PlayerPayload>) {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse(etat)
    await nextTick()
    return w
  }

  function numeros(w: Awaited<ReturnType<typeof monterAvec>>) {
    return w.findAll('[data-preset-button]').map((b) => b.text())
  }

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('s’ouvre sur la page de la présélection qui joue', async () => {
    // Le cas qui motivait tout : arriver sur l'onglet pendant que la station 24
    // joue montrait 1-10, sans aucune touche en évidence.
    const w = await monterAvec({ preset_count: 40, preset: 24 })
    expect(numeros(w)).toEqual(['21', '22', '23', '24', '25', '26', '27', '28', '29', '30'])
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
  })

  it('suit un changement de page venu d’ailleurs (télécommande infrarouge, +10)', async () => {
    const w = await monterAvec({ preset_count: 40, preset: 3 })
    expect(numeros(w)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'])
    FauxEventSource.derniere!.pousse({ preset_count: 40, preset: 31 })
    await nextTick()
    expect(numeros(w)).toEqual(['31', '32', '33', '34', '35', '36', '37', '38', '39', '40'])
  })

  it('10 et 11 sont de part et d’autre de la frontière', async () => {
    // Les bornes de la grille sont celles du décalage du cœur : une page
    // couvre 10k+1..10k+10, donc 10 clôt la page 0 et 11 ouvre la page 1. La
    // frontière était à 9/10 quand la touche 0 de la télécommande ne valait
    // rien seule.
    const w = await monterAvec({ preset_count: 40, preset: 10 })
    expect(numeros(w)[0]).toBe('1')
    FauxEventSource.derniere!.pousse({ preset_count: 40, preset: 11 })
    await nextTick()
    expect(numeros(w)[0]).toBe('11')
  })

  it('un arrêt laisse la page où elle est', async () => {
    // `preset` retombe à null sans que le compte bouge : rien ne justifie de
    // renvoyer l'utilisateur en première page, il regarde encore ce groupe.
    const w = await monterAvec({ preset_count: 40, preset: 24 })
    FauxEventSource.derniere!.pousse({ preset_count: 40, preset: null })
    await nextTick()
    expect(numeros(w)[0]).toBe('21')
  })

  it('une pagination à la main survit aux trames qui ne changent rien', async () => {
    const w = await monterAvec({ preset_count: 40, preset: 3 })
    await w.find('[data-preset-next]').trigger('click')
    await nextTick()
    expect(numeros(w)[0]).toBe('11')
    // Même présélection, même compte, seul le volume change : la page reste.
    FauxEventSource.derniere!.pousse({ preset_count: 40, preset: 3, volume: 42 })
    await nextTick()
    expect(numeros(w)[0]).toBe('11')
  })

  it('un numéro au-delà du compte n’ouvre pas une page vide', async () => {
    // Source incohérente (le compte a rétréci avant que la présélection ne
    // suive) : on borne sur la dernière page non vide plutôt que de n'afficher
    // aucune touche.
    const w = await monterAvec({ preset_count: 12, preset: 35 })
    expect(numeros(w)).toEqual(['11', '12'])
  })
})

describe('HomeView — boutons indisponibles', () => {
  async function monterAvec(etat: Partial<PlayerPayload>) {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse(etat)
    await nextTick()
    return w
  }

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('en veille, tout est grisé sauf la veille elle-même', async () => {
    // Le cœur ignore tout ce qui n'est pas `Power` en veille : les boutons le
    // disent au lieu d'envoyer une commande sans effet. Pas de compte poussé
    // ici : la veille l'efface côté cœur, la grille retombe donc sur 1-10 —
    // désactivée elle aussi.
    const w = await monterAvec({ standby: true })
    for (const b of w.findAll('[data-remote-command]')) {
      expect(b.attributes('disabled'), b.attributes('data-remote-command')).toBeDefined()
    }
    for (const b of w.findAll('[data-preset-button]')) {
      expect(b.attributes('disabled')).toBeDefined()
    }
    expect(w.get('[data-remote-source]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-remote-power]').attributes('disabled')).toBeUndefined()
  })

  it('la touche Muet montre son état, là où on agit sur le volume', async () => {
    // Demandé à l'usage : le son coupé se lisait dans l'encart, en haut de la
    // page, alors qu'on le coupe depuis cette rangée — en bas. La touche porte
    // donc son propre état, en plus de la mention de l'encart.
    const coupe = await monterAvec({ standby: false, muted: true })
    const touche = coupe.get('[data-remote-command="Mute"]')
    expect(touche.attributes('data-actif')).toBe('true')
    expect(touche.attributes('aria-pressed')).toBe('true')

    const audible = await monterAvec({ standby: false, muted: false })
    const rendue = audible.get('[data-remote-command="Mute"]')
    expect(rendue.attributes('data-actif')).toBeUndefined()
    expect(rendue.attributes('aria-pressed')).toBe('false')
  })

  it('Eject est masqué sur la radio et présent sur le lecteur de cd, disque ou pas', async () => {
    // La source le déclare elle-même — la page ne compare jamais `source` à
    // `'cd'`, ce nom venant de plugins.toml. Masqué plutôt que grisé (voir
    // `masquee`) : la radio n'a pas de tiroir, un cd sans disque en a un.
    const w = await monterAvec({ source: 'radio', can_eject: false })
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(false)
    FauxEventSource.derniere!.pousse({ can_eject: true, preset_count: 0, status: 'NO DISC' })
    await nextTick()
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })

  it('avant la première trame, rien n’est grisé', async () => {
    // La télécommande s'ouvre utilisable : griser à l'aveugle, puis dégriser,
    // ferait clignoter la carte entière à chaque ouverture d'onglet.
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })))
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    for (const b of w.findAll('[data-remote-command]')) {
      expect(b.attributes('disabled')).toBeUndefined()
    }
    expect(w.get('[data-preset-button="1"]').attributes('disabled')).toBeUndefined()
  })
})

describe('HomeView — curseurs et noms', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('le curseur de volume poste SetVolume, absolu', async () => {
    const posts: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') { posts.push(String(init.body)); return new Response(null, { status: 204 }) }
      if (url === '/api/presets') return new Response(JSON.stringify({ sources: [] }), { status: 200 })
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ volume: 60 })
    await nextTick()
    const poignee = w.get('[data-volume-curseur] [role="slider"]')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(posts).toContain(JSON.stringify({ cmd: 'SetVolume', arg: 61 }))
  })

  it('la barre poste SeekTo, du pas configure par /api/settings', async () => {
    const posts: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') { posts.push(String(init.body)); return new Response(null, { status: 204 }) }
      if (url === '/api/presets') return new Response(JSON.stringify({ sources: [] }), { status: 200 })
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ seekable: true, duration_s: 254, position_s: 87 })
    await flushPromises()
    const poignee = w.get('[data-barre] [role="slider"]')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    // Le pas vient de /api/settings (`seek_step_s: 10`, stubbe ci-dessus) :
    // 87 + 10 = 97.
    expect(posts).toContain(JSON.stringify({ cmd: 'SeekTo', arg: 97 }))
  })

  it('nomme les tuiles depuis /api/presets et recharge au changement de source', async () => {
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
    vi.stubGlobal('EventSource', FauxEventSource)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await flushPromises()
    FauxEventSource.derniere!.pousse({ source: 'radio', preset_count: 3 })
    await nextTick()
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    const avant = gets.filter((u) => u === '/api/presets').length
    FauxEventSource.derniere!.pousse({ source: 'files', preset_count: 3 })
    await flushPromises()
    expect(gets.filter((u) => u === '/api/presets').length).toBe(avant + 1)
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('tout.m3u')
  })
})

describe('indisponible / masquee', () => {
  const etat = (e: Partial<PlayerPayload>): PlayerPayload => ({
    source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
    preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
    duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
    seekable: false, can_eject: false, ...e,
  })

  it('la veille ne laisse passer que Power', () => {
    expect(indisponible('Power', etat({ standby: true }))).toBe(false)
    expect(indisponible('PlayPause', etat({ standby: true }))).toBe(true)
    expect(indisponible('Select', etat({ standby: true }))).toBe(true)
  })

  it('hors veille, rien n’est grisé : le déplacement n’a plus de touche, l’éjection se masque', () => {
    expect(indisponible('PlayPause', etat({}))).toBe(false)
    expect(indisponible('Eject', etat({ can_eject: false }))).toBe(false)
  })

  it('Eject est masqué tant que la source ne déclare pas de tiroir, y compris avant la première trame', () => {
    // `can_eject` est une capacité que le greffon déclare pour lui-même (le cd
    // la déclare disque ou pas) : la masquer ne cache jamais un lecteur qui
    // existe. Avant la première trame, on ne sait pas — donc rien.
    expect(masquee('Eject', null)).toBe(true)
    expect(masquee('Eject', etat({ can_eject: false }))).toBe(true)
    expect(masquee('Eject', etat({ can_eject: true }))).toBe(false)
    expect(masquee('Stop', etat({ can_eject: false }))).toBe(false)
  })

  it('un état inconnu ne grise rien', () => {
    expect(indisponible('PlayPause', null)).toBe(false)
  })
})
