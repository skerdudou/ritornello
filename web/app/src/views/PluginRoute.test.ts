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
  props: ['name', 'catalog', 'cause', 'catalogPending'],
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
          : Promise.resolve(new Response(JSON.stringify({ from: 'generic-input' }), { status: 200 })),
      ),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises() // the radio catalog is still in flight

    route.params.name = 'generic-input'
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({ from: 'generic-input' })

    // The radio response finally arrives: it is stale, nothing must move.
    deliverRadio(new Response(JSON.stringify({ from: 'radio' }), { status: 200 }))
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalog')).toEqual({ from: 'generic-input' })
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

  // --- Telling the view that the catalog is still in flight ---
  //
  // Without this signal the view mounts the plugin's component right after its
  // module arrives, hence often before the labels: the page then shows the
  // translation keys and swaps them for the real wording a moment later. Since
  // the two do not have the same length, every label of the page shifts.

  it('declares the catalog in flight, then settled once it lands', async () => {
    let deliver: (r: Response) => void = () => {}
    vi.stubGlobal(
      'fetch',
      vi.fn(() => new Promise<Response>((res) => (deliver = res))),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalogPending')).toBe(true)

    deliver(new Response(JSON.stringify({ btn: 'Save' }), { status: 200 }))
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalogPending')).toBe(false)
  })

  it('counts a refused catalog as settled', async () => {
    // A refusal is a final answer. Leaving the flag raised would hold the
    // curtain shut for good on a plugin whose UI is otherwise perfectly able
    // to show — and to display the cause of that very refusal.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ error: 'dead' }), { status: 502 })),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalogPending')).toBe(false)
  })

  it('reopens the wait when another plugin is navigated to', async () => {
    // The flag has to go back up on the spot, before the new request is even
    // sent: were it left down, the incoming page would be revealed carrying
    // the **previous** plugin's catalog — the stale-catalog defect the
    // generation counter fixes, arriving through the other door.
    let deliver: (r: Response) => void = () => {}
    vi.stubGlobal(
      'fetch',
      vi.fn((url: string) =>
        String(url).startsWith('/plugins/radio/')
          ? Promise.resolve(new Response('{}', { status: 200 }))
          : new Promise<Response>((res) => (deliver = res)),
      ),
    )
    const PluginRoute = (await import('./PluginRoute.vue')).default
    const w = mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalogPending')).toBe(false)

    route.params.name = 'generic-input'
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalogPending')).toBe(true)

    deliver(new Response('{}', { status: 200 }))
    await flushPromises()
    expect(w.findComponent(PluginViewStub).props('catalogPending')).toBe(false)
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

  // --- Versioned catalog URL ---
  //
  // The gain this whole chantier was built for: a plugin catalog served
  // `immutable` under `?lang=<locale>&v=<session>` (Task 8) is only useful
  // once the shell actually asks for it under that URL. Two tests: the
  // unstamped fallback first, while `usePlugins()`'s module state is still
  // pristine (neither test below calls `refresh()` before this one) — order
  // matters here, since this file does not reset modules between tests.

  it('requests the catalog under a bare URL while the session is still unknown', async () => {
    // `/api/status` may not have answered yet when the first plugin page
    // mounts. A URL carrying an empty `v=` would be cached forever under a
    // false stamp, so the fallback is no query at all, not a half-stamped one.
    vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 200 })))
    const PluginRoute = (await import('./PluginRoute.vue')).default
    mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    const url = String(vi.mocked(fetch).mock.calls[0]![0])
    expect(url).toBe('/plugins/radio/api/i18n')
  })

  it('asks for the catalog in an explicit language, under a versioned URL', async () => {
    // Changing language then becomes a change of URL — the browser's own
    // cache does the invalidation, and nothing has to be purged by hand. It
    // is the systematic refetch that plays that role today.
    //
    // In real use, `App.vue`'s `onMounted` has already read `/api/status`
    // before a plugin page can be navigated to; simulated here by seeding
    // `usePlugins()` directly, since this test mounts only `PluginRoute`.
    vi.stubGlobal(
      'fetch',
      vi.fn((url: string) =>
        String(url).startsWith('/api/status')
          ? Promise.resolve(
              new Response(
                JSON.stringify({ plugins: [], active_source: '', session: 'sess-1', locale: 'fr' }),
                { status: 200 },
              ),
            )
          : Promise.resolve(new Response('{}', { status: 200 })),
      ),
    )
    const { usePlugins } = await import('../composables/usePlugins')
    await usePlugins().refresh()
    const PluginRoute = (await import('./PluginRoute.vue')).default
    mount(PluginRoute, { global: { stubs: { PluginView: PluginViewStub } } })
    await flushPromises()
    const call = vi.mocked(fetch).mock.calls.find((c) => String(c[0]).startsWith('/plugins/radio/'))
    const url = String(call![0])
    expect(url).toMatch(/^\/plugins\/radio\/api\/i18n\?/)
    expect(url).toContain('lang=fr')
    expect(url).toContain('v=sess-1')
  })
})
