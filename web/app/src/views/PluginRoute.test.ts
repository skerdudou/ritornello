import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'

// Fake route driven by the test: it is the only piece of vue-router the
// component consumes.
const route = reactive({ params: { name: 'radio' } as { name?: string } })
vi.mock('vue-router', () => ({ useRoute: () => route }))

// The real PluginView loads a remote ESM module: off topic here, we only
// check what PluginRoute passes to it.
const PluginViewStub = {
  props: ['name', 'catalog', 'cause'],
  template: '<div data-stub />',
}

describe('PluginRoute', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    route.params.name = 'radio'
  })

  it('a late catalog does not replace that of the displayed plugin', async () => {
    // Regression (review 2026-07-27): fast navigation radio → generic-input
    // with a slow radio i18n GET — the radio response arrived after that of
    // generic-input and settled under the displayed admin. Same class of
    // defect as the one PluginView fixes for the module.
    let deliverRadio: (r: Response) => void = () => {}
    const radioResponse = new Promise<Response>((res) => {
      deliverRadio = res
    })
    vi.stubGlobal(
      'fetch',
      vi.fn((url: string) =>
        String(url).startsWith('/plugins/radio/')
          ? radioResponse
          : Promise.resolve(new Response(JSON.stringify({ qui: 'generic-input' }), { status: 200 })),
      ),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises() // the radio catalog is still in flight

    route.params.name = 'generic-input'
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({ qui: 'generic-input' })

    // The radio response finally arrives: it is stale, nothing must move.
    deliverRadio(new Response(JSON.stringify({ qui: 'radio' }), { status: 200 }))
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({ qui: 'generic-input' })
  })

  it('passes on the cause carried by a refusal of the core instead of swallowing it', async () => {
    // On the first load of a page whose plugin is dead, the screen only showed
    // "plugin UI unavailable": the cause went into a `console.warn`. Yet the
    // core now carries it in the body of its 502s ("the plugin took more than
    // 5 s to answer…"), and that is the only channel giving it — the module,
    // for its part, is loaded with `import()`, whose failure delivers no
    // usable body.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        new Response(JSON.stringify({ error: 'the plugin took more than 5 s to answer' }), {
          status: 502,
        }),
      ),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('cause')).toBe(
      'the plugin took more than 5 s to answer',
    )
    // And the catalog stays empty: `t()` falls back on the keys, which stays
    // readable. A catalog refusal does not prevent the page from showing.
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({})
  })

  it('passes no cause when the catalog arrives', async () => {
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
