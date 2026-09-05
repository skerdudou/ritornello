import { SKELETON_DELAY_MS, SKELETON_MIN_VISIBLE_MS, type Catalog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
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

/**
 * Whether the plugin's component is mounted but kept off screen.
 *
 * The view mounts it as soon as it exists and hides it until the wait is over,
 * so "not on screen" and "not mounted" are two different states — and only the
 * `display: none` rule tells them apart.
 */
function hiddenContent(w: ReturnType<typeof mountView>): boolean {
  return w.get('[data-plugin-content]').attributes('style') === 'display: none;'
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

  // Several tests below fake the clock to walk through the loading rhythm. A
  // fake clock left installed would leak into the next file's timers, so it is
  // handed back unconditionally — a no-op when it was never taken.
  afterEach(() => {
    vi.useRealTimers()
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
    // The clock is faked **before** mounting: the wait timer is armed during
    // mount, and a fake clock installed afterwards would never own it.
    vi.useFakeTimers()
    // The three states that each produce one of the three messages.
    const view = defineComponent({ render: () => h('p', 'ui') })
    const neverResolved = mountView(() => new Promise<never>(() => {})) // stays loading
    const contractKo = mountView(async () => ({ contract: 99, default: view }), 'contract-ko')
    const unreachable = mountView(async () => {
      throw new Error('404')
    }, 'unreachable')
    await flushPromises()
    // `loading` is no longer shown on sight: the page stays blank for the
    // first fraction of a second, then announces the wait. The clock has to be
    // pushed past that threshold for the first of the three texts to exist at
    // all — the behaviour the tests further down pin down in its own right.
    vi.advanceTimersByTime(SKELETON_DELAY_MS)
    await flushPromises()

    const texts = [neverResolved.text(), contractKo.text(), unreachable.text()]
    expect(texts).toEqual(['Loading…', 'Plugin to rebuild', 'UI unavailable'])
    // The real invariant: no displayed text must equal its key.
    for (const text of texts) {
      expect(keys).not.toContain(text)
    }
  })

  // --- The loading rhythm: nothing, then a placeholder, and never a page
  // that assembles itself under the reader's eyes ---

  describe('loading rhythm', () => {
    beforeEach(() => {
      // Armed before every mount of this block, for the reason above.
      vi.useFakeTimers()
    })

    it('shows strictly nothing while the wait is short', async () => {
      // The whole point of the delay: on this installation the module and its
      // catalog arrive in a few dozen milliseconds, so the normal experience
      // must be an empty frame followed by a complete page — never a grey
      // flash, and never a half-built one.
      const w = mountView(() => new Promise<never>(() => {}))
      await flushPromises()
      vi.advanceTimersByTime(SKELETON_DELAY_MS - 1)
      await flushPromises()

      expect(w.text()).toBe('')
      expect(w.find('[data-slot="skeleton"]').exists()).toBe(false)
    })

    it('paints a placeholder once the wait outlasts the delay', async () => {
      const w = mountView(() => new Promise<never>(() => {}))
      await flushPromises()
      vi.advanceTimersByTime(SKELETON_DELAY_MS)
      await flushPromises()

      expect(w.find('[data-slot="skeleton"]').exists()).toBe(true)
      // The wait is announced once, on the container: the placeholder blocks
      // themselves are `aria-hidden`, so a screen reader hears one message
      // rather than a run of empty boxes.
      const status = w.get('[role="status"]')
      expect(status.text()).toBe('Loading…')
    })

    it('does not even build the plugin while its catalog is unsettled', async () => {
      // **This is the i18n jump, and more than that.** The module can be ready
      // while the plugin's catalog is still in flight; mounting then shows raw
      // keys (`col_num`, `btn_save`) which the real labels later replace — and
      // those do not have the same length, so every label shifts.
      //
      // This test used to assert the component was mounted and merely hidden.
      // That was not enough, and a bug reported from use is what proved it: a
      // `display: none` curtain hides a component that is fully mounted and
      // running, and the kit's `SelectItemText` hands its option's text to the
      // Select in `onMounted`, once. A dropdown built behind the curtain
      // registered the raw key and still showed it when the curtain lifted.
      // Hence a real absence, checked here through the component's `setup`
      // rather than through what is on screen — the whole point is that
      // nothing of it runs.
      const built: number[] = []
      const view = defineComponent({
        setup: () => {
          built.push(1)
          return () => h('p', 'plugin UI')
        },
      })
      const w = mount(PluginView, {
        props: {
          name: 'demo',
          loadModule: async () => ({ contract: 1, default: view }),
          catalog: CATALOG,
          catalogPending: true,
        },
      })
      await flushPromises()
      expect(built).toHaveLength(0)
      expect(w.find('[data-plugin-content]').exists()).toBe(false)

      await w.setProps({ catalogPending: false })
      await flushPromises()
      expect(built).toHaveLength(1)
      expect(hiddenContent(w)).toBe(false)
      expect(w.text()).toContain('plugin UI')
    })

    it('builds the plugin even when its catalog request failed', async () => {
      // "Settled" means answered **or** refused. `PluginRoute` lowers the flag
      // on a refusal too, handing down an empty catalog: the page then shows
      // keys, which is bad, but a page withheld forever would be worse. The
      // guarantee this view offers a plugin author is therefore "your
      // component is never built with an *unsettled* catalog" — never "with a
      // translated one".
      const built: number[] = []
      const view = defineComponent({
        setup: () => {
          built.push(1)
          return () => h('p', 'plugin UI')
        },
      })
      const w = mount(PluginView, {
        props: {
          name: 'demo',
          loadModule: async () => ({ contract: 1, default: view }),
          catalog: {},
          catalogPending: false,
        },
      })
      await flushPromises()
      expect(built).toHaveLength(1)
      expect(w.text()).toContain('plugin UI')
    })

    it('with its catalog settled, mounts behind the placeholder so its loading starts at once', async () => {
      // What the `display: none` curtain is still for, now that an unsettled
      // catalog withholds the mount outright (see the test above): the floor
      // keeps the placeholder on screen for a moment after the module has
      // arrived, and a component only mounted once that floor expired would
      // start its own fetch up to 300 ms late — pure latency added to the slow
      // path, the one case where it hurts — then paint its own placeholder
      // after a blank gap. With the catalog already settled, it is mounted
      // underneath for that tail instead: hidden, but working.
      const mounted: number[] = []
      let resolve: (m: unknown) => void = () => {}
      const view = defineComponent({
        setup: () => {
          mounted.push(Date.now())
          return () => h('p', 'plugin UI')
        },
      })
      const w = mountView(() => new Promise((r) => (resolve = r)))
      await flushPromises()
      vi.advanceTimersByTime(SKELETON_DELAY_MS)
      await flushPromises()

      // The module lands while the placeholder is still serving its minimum.
      resolve({ contract: 1, default: view })
      await flushPromises()
      expect(w.find('[data-slot="skeleton"]').exists()).toBe(true)
      expect(mounted).toHaveLength(1)

      // And it is the same instance that surfaces: a component mounted twice
      // would fire its data request twice.
      vi.advanceTimersByTime(SKELETON_MIN_VISIBLE_MS)
      await flushPromises()
      expect(w.find('[data-slot="skeleton"]').exists()).toBe(false)
      expect(w.text()).toContain('plugin UI')
      expect(mounted).toHaveLength(1)
    })

    it('once shown, the placeholder is not wiped by data landing just after', async () => {
      // Without the floor, a module that resolves 10 ms after the delay
      // elapsed paints a grey flash and erases it — strictly worse than the
      // jump the placeholder replaced.
      let resolve: (m: unknown) => void = () => {}
      const view = defineComponent({ render: () => h('p', 'plugin UI') })
      const w = mountView(() => new Promise((r) => (resolve = r)))
      await flushPromises()
      vi.advanceTimersByTime(SKELETON_DELAY_MS)
      await flushPromises()
      expect(w.find('[data-slot="skeleton"]').exists()).toBe(true)

      vi.advanceTimersByTime(10)
      resolve({ contract: 1, default: view })
      await flushPromises()

      expect(w.find('[data-slot="skeleton"]').exists()).toBe(true)
      expect(hiddenContent(w)).toBe(true)
    })
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

  it('stamps the stylesheet with the version the plugin announced', async () => {
    // A stable name is the plugin UI contract and cannot carry a hash: the
    // fingerprint travels in the query, and that is what lets the sheet be
    // cached for good instead of revalidated on every load.
    document.head.innerHTML = ''
    const view = defineComponent({ render: () => h('p', 'ok') })
    mount(PluginView, {
      props: {
        name: 'stamped-sheet',
        loadModule: async () => ({ contract: 1, default: view }),
        catalog: CATALOG,
        uiVersion: 'cafe',
      },
    })
    await flushPromises()
    const style = document.head.querySelector('style[data-plugin-sheet="stamped-sheet"]')
    expect(style?.textContent).toBe(
      '@import url("/plugins/stamped-sheet/ui.css?v=cafe") layer(plugin);',
    )
  })

  it('falls back to the plain URL when no version was announced', async () => {
    // A plugin predating the field, or one without assets: the old behaviour
    // (revalidation) must keep working rather than produce a broken URL.
    document.head.innerHTML = ''
    const view = defineComponent({ render: () => h('p', 'ok') })
    mountView(async () => ({ contract: 1, default: view }), 'plain-sheet')
    await flushPromises()
    const style = document.head.querySelector('style[data-plugin-sheet="plain-sheet"]')
    expect(style?.textContent).toBe('@import url("/plugins/plain-sheet/ui.css") layer(plugin);')
  })

  // --- Final review, Important 1: the module was loaded twice on a direct
  // page load ---
  //
  // `<RouterView/>` mounts this view before `/api/status` has answered, so
  // `props.uiVersion` first holds `''`, then the real fingerprint once
  // `/api/status` settles. A `watchEffect` reacting to that change imports
  // `ui.js` under two different URLs, hence evaluates the plugin's module
  // twice, and `ensureStylesheet` appends a second `<style
  // data-plugin-sheet="…">` (keyed by name **and** version) whose rules keep
  // applying because the first is never removed.

  it('loads the module once when the status settles after mount, not twice', async () => {
    document.head.innerHTML = ''
    const view = defineComponent({ render: () => h('p', 'ok') })
    const loader = vi.fn(async () => ({ contract: 1, default: view }))
    const w = mount(PluginView, {
      props: {
        name: 'settle-once',
        loadModule: loader,
        catalog: CATALOG,
        uiVersion: '',
        statusPending: true,
      },
    })
    await flushPromises()
    // The curtain is held: nothing has been imported yet, on the version that
    // will be discarded a moment later.
    expect(loader).not.toHaveBeenCalled()

    // `/api/status` settles, `PluginRoute` relays the real fingerprint.
    await w.setProps({ uiVersion: 'v2', statusPending: false })
    await flushPromises()

    expect(loader).toHaveBeenCalledTimes(1)
    expect(loader).toHaveBeenCalledWith('settle-once', 'v2')
    expect(
      document.head.querySelectorAll('style[data-plugin-sheet="settle-once"]'),
    ).toHaveLength(1)
  })

  it('lifts the curtain and uses the bare URL when /api/status never settles', async () => {
    // "Settled" means answered *or* failed — never an indefinite wait. If
    // `/api/status` is unreachable, `usePlugins` reports it (`unavailable`)
    // and `PluginRoute` must lower `statusPending` anyway, exactly as for a
    // plugin that announced no fingerprint. A plugin page that never appears
    // because `/api/status` is down would be far worse than an uncached
    // asset.
    document.head.innerHTML = ''
    const view = defineComponent({ render: () => h('p', 'ok') })
    const loader = vi.fn(async () => ({ contract: 1, default: view }))
    const w = mount(PluginView, {
      props: {
        name: 'status-down',
        loadModule: loader,
        catalog: CATALOG,
        uiVersion: '',
        statusPending: true,
      },
    })
    await flushPromises()
    expect(loader).not.toHaveBeenCalled()

    await w.setProps({ statusPending: false }) // uiVersion stays '': /api/status failed
    await flushPromises()

    expect(loader).toHaveBeenCalledTimes(1)
    expect(loader).toHaveBeenCalledWith('status-down', '')
  })
})
