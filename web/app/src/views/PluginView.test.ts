import type { Catalog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { defineComponent, h, type PropType } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import PluginView from './PluginView'

// The three keys live in the **common** vocabulary
// (`crates/ritornello-i18n/src/locales/common_en.toml`,
// `deploy/locales/common/fr.toml`), hence inherited by every catalog — the
// core's as well as each plugin's.
const CATALOG = {
  plugin_unavailable: 'UI unavailable',
  plugin_unavailable_cause: 'UI unavailable: {cause}',
  plugin_contract_mismatch: 'Plugin to rebuild',
  loading: 'Loading…',
}

function mountView(loader: () => Promise<unknown>, name = 'demo', cause = '') {
  return mount(PluginView, { props: { name, loadModule: loader, catalog: CATALOG, cause } })
}

describe('PluginView', () => {
  // The shell's catalog lives in a module-level `ref` of `useCatalog`: it
  // persists between `it()`s. We reset it to empty before each test so that
  // only the tests that populate it explicitly benefit from it.
  beforeEach(async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 200 })))
    await useCatalog().reload()
    vi.unstubAllGlobals()
  })

  it('mounts the module component when the contract matches', async () => {
    const view = defineComponent({ render: () => h('p', 'plugin UI') })
    const w = mountView(async () => ({ contract: 1, default: view }))
    await flushPromises()
    expect(w.text()).toContain('plugin UI')
  })

  it('passes the catalog to the mounted component', async () => {
    // Regression: `h(component.value)` alone does not forward PluginView's
    // props (this is not attribute fallthrough, which only concerns undeclared
    // attributes) — every real plugin module declaring `catalog` as a required
    // prop (RadioAdmin, InputAdmin) received `undefined` and `createT` threw
    // at the first `t(...)` of its template.
    const view = defineComponent({
      props: { catalog: { type: Object as PropType<Catalog>, required: true } },
      render(this: { catalog: Catalog }) {
        return h('p', this.catalog.key ?? 'catalog missing')
      },
    })
    const w = mount(PluginView, {
      props: {
        name: 'demo-catalog',
        loadModule: async () => ({ contract: 1, default: view }),
        catalog: { key: 'value passed' },
      },
    })
    await flushPromises()
    expect(w.text()).toContain('value passed')
  })

  it('passes the absolute prefix `base` to the mounted component', async () => {
    // IMPORTANT 6 of the final review: `base` is part of the contract of
    // plugin UIs, just like `catalog`. The modules built their URLs relatively
    // (`./api/data`), hence resolved against the browser's URL — a silent
    // coupling to the form (trailing slash or not) of the shell's route.
    const view = defineComponent({
      props: { base: { type: String, required: true } },
      render(this: { base: string }) {
        return h('p', this.base)
      },
    })
    const w = mount(PluginView, {
      props: {
        name: 'radio',
        loadModule: async () => ({ contract: 1, default: view }),
        catalog: CATALOG,
      },
    })
    await flushPromises()
    // **Absolute** prefix, trailing slash included: the modules concatenate
    // directly (`${base}api/data`).
    expect(w.text()).toBe('/plugins/radio/')
  })

  it('refuses an incompatible contract with an explicit message', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const view = defineComponent({ render: () => h('p', 'must not appear') })
    const w = mountView(async () => ({ contract: 99, default: view }))
    await flushPromises()
    expect(w.text()).toContain('Plugin to rebuild')
    expect(w.text()).not.toContain('must not appear')
    // Diagnostic specific to this branch: distinguishes this case from the
    // loading failure and from the missing default export, which share the
    // same message displayed on screen ('UI unavailable' is not concerned
    // here, but the principle holds for the next two tests).
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('contract 99 expected 1'))
  })

  it('shows the unavailability when the module does not load', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = mountView(async () => {
      throw new Error('404')
    })
    await flushPromises()
    expect(w.text()).toContain('UI unavailable')
    // The displayed message is identical to that of the next test: it is the
    // `console.warn` that really distinguishes the two branches.
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining('loading failed'),
      expect.anything(),
    )
  })

  it('shows the unavailability when the module exports no component', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = mountView(async () => ({ contract: 1 }))
    await flushPromises()
    expect(w.text()).toContain('UI unavailable')
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('no default component'))
  })

  it('names the cause of the refusal when the route collected it', async () => {
    // The module is loaded with `import()`, whose failure delivers no usable
    // body: the cause can only come from the call to `api/i18n`, a `fetch`
    // whose body can be read. `PluginRoute` collects it and passes it here.
    // Without it, the screen said "UI unavailable" and nothing more, at the
    // moment one most needs to know why.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = mountView(
      async () => {
        throw new Error('404')
      },
      'demo',
      'the plugin took more than 5 s to answer',
    )
    await flushPromises()
    expect(w.text()).toBe('UI unavailable: the plugin took more than 5 s to answer')
  })

  it('without a known cause, the generic message stays as is', async () => {
    // The module can fail while the plugin answers perfectly well: a missing
    // `dist`, a mismatched contract. Inventing a cause would be worse than
    // giving none.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = mountView(async () => {
      throw new Error('404')
    })
    await flushPromises()
    expect(w.text()).toBe('UI unavailable')
  })

  it('a cause is not appended to a mismatched contract', async () => {
    // That message already says what to do (rebuild the plugin's UI), and the
    // cause of a catalog refusal has nothing to do with it.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const view = defineComponent({ render: () => h('p', 'ui') })
    const w = mountView(async () => ({ contract: 99, default: view }), 'demo', 'whatever')
    await flushPromises()
    expect(w.text()).toBe('Plugin to rebuild')
  })

  // --- IMPORTANT 4 of the final review: the three messages were shown as raw
  // keys ---
  //
  // `t('unavailable')`, `t('contract')` and `t('loading')` matched NO key of
  // any catalog (neither `common_en.toml`, nor the core's English, nor the
  // packs of `deploy/locales/`). `createT` falling back on the key, the user
  // literally read "loading" then "unavailable" or "contract" — and "contract"
  // said nothing of the "translated message indicating that the plugin must
  // be rebuilt" required by the spec.
  it('none of the three messages is shown as a raw key', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const keys = ['loading', 'plugin_unavailable', 'plugin_contract_mismatch'] as const
    // The three states that each produce one of the three messages.
    const view = defineComponent({ render: () => h('p', 'ui') })
    const neverResolved = mountView(() => new Promise<never>(() => {})) // stays loading
    const contractKo = mountView(async () => ({ contract: 99, default: view }), 'contract-ko')
    const unreachable = mountView(async () => {
      throw new Error('404')
    }, 'unreachable')
    await flushPromises()

    const texts = [neverResolved.text(), contractKo.text(), unreachable.text()]
    expect(texts).toEqual(['Loading…', 'Plugin to rebuild', 'UI unavailable'])
    // The real invariant: no displayed text must equal its key.
    for (const text of texts) {
      expect(keys).not.toContain(text)
    }
  })

  it('unreachable plugin (empty catalog): the message comes from the shell catalog', async () => {
    // The worst case of the original diagnosis: those keys were resolved in
    // **the plugin's** catalog, empty precisely when the plugin is unreachable
    // — the case that produces `plugin_unavailable`. The fallback on the
    // shell's catalog (`useCatalog`) is therefore what makes the message
    // readable in the only case where it really matters.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ plugin_unavailable: 'Plugin unreachable' }), { status: 200 })),
    )
    await useCatalog().reload() // the shell has its catalog (loaded by App.vue)

    // `catalog: {}`: this is exactly what `PluginRoute.vue` passes when
    // `GET /plugins/<name>/api/i18n` fails.
    const w = mount(PluginView, {
      props: {
        name: 'empty-catalog',
        loadModule: async () => {
          throw new Error('404')
        },
        catalog: {},
      },
    })
    await flushPromises()
    expect(w.text()).toBe('Plugin unreachable')
  })

  it('injects the plugin stylesheet only once', async () => {
    document.head.innerHTML = ''
    const view = defineComponent({ render: () => h('p', 'ok') })
    // Name dedicated to this test (rather than the 'demo' shared by the
    // others): the tracking of injected sheets lives in a module-level `Set`
    // of `PluginView.ts`, which persists between the `it()`s of this file —
    // reusing 'demo' would make the result depend on execution order.
    mountView(async () => ({ contract: 1, default: view }), 'single-sheet')
    mountView(async () => ({ contract: 1, default: view }), 'single-sheet')
    await flushPromises()
    expect(document.head.querySelectorAll('style[data-plugin-sheet="single-sheet"]')).toHaveLength(1)
  })

  it('files the plugin sheet in the `plugin` layer', async () => {
    // **The regression targeted is the top menu disappearing.** Both Tailwind
    // passes (shell and plugin) wrote into the same `utilities` layer; the
    // plugin's sheet, injected later, won at equal specificity, and its
    // `.hidden` (InputAdmin's file field) overwrote the `md:flex` of
    // `data-top-nav`. Without this assertion, going back to a
    // `<link rel="stylesheet">` would break no other test.
    document.head.innerHTML = ''
    const view = defineComponent({ render: () => h('p', 'ok') })
    mountView(async () => ({ contract: 1, default: view }), 'layered-sheet')
    await flushPromises()
    const style = document.head.querySelector('style[data-plugin-sheet="layered-sheet"]')
    expect(style?.textContent).toBe(
      '@import url("/plugins/layered-sheet/ui.css") layer(plugin);',
    )
  })
})
