import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'

// Route factice pilotable par le test : c'est le seul morceau de vue-router
// que le composant consomme.
const route = reactive({ params: { name: 'radio' } as { name?: string } })
vi.mock('vue-router', () => ({ useRoute: () => route }))

// Le vrai PluginView load un module ESM distant : hors sujet ici, on ne
// vérifie que ce que PluginRoute lui transmet.
const PluginViewStub = {
  props: ['name', 'catalog', 'cause'],
  template: '<div data-stub />',
}

describe('PluginRoute', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    route.params.name = 'radio'
  })

  it('un catalogue en retard ne remplace step celui du plugin affiché', async () => {
    // Régression (revue 2026-07-27) : navigation rapide radio → generic-input
    // avec un GET i18n radio lent — la réponse de radio arrivait après celle
    // de generic-input et s'installait sous l'admin affichée. Même classe de
    // défaut que celle que PluginView corrige pour le module.
    let livrerRadio: (r: Response) => void = () => {}
    const reponseRadio = new Promise<Response>((res) => {
      livrerRadio = res
    })
    vi.stubGlobal(
      'fetch',
      vi.fn((url: string) =>
        String(url).startsWith('/plugins/radio/')
          ? reponseRadio
          : Promise.resolve(new Response(JSON.stringify({ qui: 'generic-input' }), { status: 200 })),
      ),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises() // le catalogue de radio est toujours en vol

    route.params.name = 'generic-input'
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({ qui: 'generic-input' })

    // La réponse de radio arrive enfin : elle est périmée, rien ne doit bouger.
    livrerRadio(new Response(JSON.stringify({ qui: 'radio' }), { status: 200 }))
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({ qui: 'generic-input' })
  })

  it('transmet la cause portée par un refus du cœur au lieu de l’avaler', async () => {
    // Au premier chargement d'une page dont le plugin est mort, l'écran
    // n'affichait qu'« IHM du plugin unavailable » : la cause partait dans un
    // `console.warn`. Or le cœur la porte désormais dans le corps de ses 502
    // (« le plugin a mis plus de 5 s à répondre… »), et c'est le seul canal qui
    // la donne — le module, lui, est chargé par `import()`, dont l'échec ne
    // livre aucun corps exploitable.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        new Response(JSON.stringify({ error: 'le plugin a mis plus de 5 s a repondre' }), {
          status: 502,
        }),
      ),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('cause')).toBe(
      'le plugin a mis plus de 5 s a repondre',
    )
    // Et le catalogue reste vide : `t()` retombe sur les clés, ce qui reste
    // lisible. Un refus de catalogue n'empêche step la page de s'afficher.
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({})
  })

  it('ne transmet aucune cause quand le catalogue arrive', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ btn: 'Enregistrer' }), { status: 200 })),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('cause')).toBe('')
  })
})
