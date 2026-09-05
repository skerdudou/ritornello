import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App.vue'
import { resetCatalog, useCatalog } from './composables/useCatalog'
import { resetMetrics } from './composables/useMetrics'
import { router } from './router'

// The visual marker of the current page: the class `exact-active-class` adds
// to the single exact link. Pinning it verbatim is deliberate — it is the
// underline mechanism, and swapping it for something else must make this test
// fail rather than go unnoticed.
const UNDERLINED = 'after:scale-x-100'

const CATALOG = { config_title: 'Configuration', system_title: 'Système' }

/** `/api/i18n` on one side, `/api/status` on the other — the nav reads nothing
 *  else. `/api/system` joins them since the root starts the metrics probing:
 *  the served payload matters little (no jiffy, no memory, hence no sample
 *  pushed), only the existence of the call counts. */
function stub(plugins = [{ name: 'radio', admin: true }]) {
  const f = vi.fn().mockImplementation((url: string) =>
    Promise.resolve({
      ok: true,
      json: async () => (String(url).includes('/api/i18n') ? CATALOG : { plugins }),
    } as Response),
  )
  vi.stubGlobal('fetch', f)
  return f
}

/**
 * Mounts the shell on `path`. `RouterView` is stubbed: only the nav is under
 * test here, and mounting the real views would make `HomeView` open its
 * `EventSource` stream, which jsdom does not implement.
 */
async function mountAt(path: string) {
  stub()
  await router.push(path)
  await router.isReady()
  const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
  await flushPromises()
  return w
}

describe('shell navigation', () => {
  afterEach(() => {
    // `catalog` and `settled` live at module level: without this, the first
    // test to let a catalog land leaves every later one believing the shell
    // is already ready, and the two tests below would pass for the wrong
    // reason.
    resetCatalog()
    resetMetrics()
    vi.unstubAllGlobals()
  })

  it('underlines only the link of the current page', async () => {
    const w = await mountAt('/system')
    expect(w.get('a[href="/system"]').classes()).toContain(UNDERLINED)
    expect(w.get('a[href="/config"]').classes()).not.toContain(UNDERLINED)
    // The home link is the one that would make `active-class` unusable: the
    // router's inclusive matching holds it active on every page, `/` being a
    // prefix of everything. Hence `exact-active-class`.
    expect(w.get('a[href="/"]').classes()).not.toContain(UNDERLINED)
    w.unmount()
  })

  it('underlines the brand on the home page', async () => {
    // Without it, home would be the only page with nothing underlined: it is
    // the home link as much as the brand.
    const w = await mountAt('/')
    expect(w.get('a[href="/"]').classes()).toContain(UNDERLINED)
    expect(w.get('a[href="/system"]').classes()).not.toContain(UNDERLINED)
    w.unmount()
  })

  it('underlines the link of an admin plugin on its page', async () => {
    const w = await mountAt('/plugins/radio/')
    expect(w.get('a[href="/plugins/radio/"]').classes()).toContain(UNDERLINED)
    expect(w.get('a[href="/"]').classes()).not.toContain(UNDERLINED)
    w.unmount()
  })

  it('marks the current page for screen readers, not only for the eye', async () => {
    // `aria-current="page"` comes from `RouterLink` itself: the underline
    // duplicates that semantics, it does not replace it. The test pins it so
    // that nobody "simplifies" the nav into bare links later.
    const w = await mountAt('/config')
    expect(w.get('a[href="/config"]').attributes('aria-current')).toBe('page')
    expect(w.get('a[href="/system"]').attributes('aria-current')).toBeUndefined()
    w.unmount()
  })

  it('produces a single link for a plugin announced under several kinds', async () => {
    // The core pushes one status line per (name, kind): an `mpd` plugin as
    // `input` + `display` with an admin page yields two lines with the same
    // name, both `admin: true`. The nav must derive only one link from them,
    // on pain of two `RouterLink`s on the same key (Vue duplicate-keys
    // warning).
    stub([
      { name: 'mpd', admin: true },
      { name: 'mpd', admin: true },
    ])
    await router.push('/')
    await router.isReady()
    const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
    await flushPromises()
    // Scoped to the top nav: the bottom bar also points at `/plugins/mpd/`
    // when there is a single admin plugin (see `BottomNav.test.ts`), which is
    // a legitimate second link and not a duplicate from the same `v-for`.
    expect(w.get('[data-top-nav]').findAll('a[href="/plugins/mpd/"]')).toHaveLength(1)
    w.unmount()
  })

  it('starts the metrics probing when the SPA mounts', async () => {
    // The history must exist before the first visit to the system tab: the
    // view displays it, it no longer collects it. `RouterView` is stubbed
    // here, so `SystemView` is never mounted — it really is the root that
    // starts it.
    const f = stub()
    await router.push('/')
    await router.isReady()
    const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
    await flushPromises()
    expect(f.mock.calls.some((c) => String(c[0]).includes('/api/system'))).toBe(true)
    w.unmount()
  })

  it('hides the top nav below md and renders the bottom bar', async () => {
    const w = await mountAt('/') // the file's existing mount helper
    expect(w.get('[data-top-nav]').classes()).toContain('hidden')
    expect(w.get('[data-top-nav]').classes()).toContain('md:flex')
    expect(w.find('[data-bottom-nav]').exists()).toBe(true)
    w.unmount()
  })

  it('withholds the routed view until the catalog has come back', async () => {
    // The defect this exists to forbid, reported on the configuration page:
    // a view mounted before the catalog renders translation keys, and while
    // most labels recover on the next render, a dropdown does not — the
    // kit's `SelectItemText` hands its text to the Select once, at mount.
    // `audio_default_device` therefore stayed on screen for the life of the
    // page. Holding the view back is what fixes every list at once.
    let release: (v: Response) => void = () => {}
    const pending = new Promise<Response>((resolve) => {
      release = resolve
    })
    vi.stubGlobal(
      'fetch',
      vi.fn((url: string) =>
        String(url).includes('/api/i18n')
          ? pending
          : Promise.resolve({ ok: true, json: async () => ({ plugins: [] }) } as Response),
      ),
    )
    await router.push('/')
    await router.isReady()
    const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
    await flushPromises()
    expect(w.find('router-view-stub').exists()).toBe(false)
    // The chrome is deliberately not held back: its labels recover on their
    // own, and hiding it would trade a fixed defect for a bigger jump.
    expect(w.find('[data-bottom-nav]').exists()).toBe(true)

    release({ ok: true, json: async () => CATALOG } as Response)
    await flushPromises()
    expect(w.find('router-view-stub').exists()).toBe(true)
    w.unmount()
  })

  it('reveals the routed view even when the catalog request fails', async () => {
    // "Settled" must mean answered **or** failed. A page withheld forever
    // because `/api/i18n` is down would be far worse than one showing keys —
    // the same rule `PluginView` already applies to `/api/status`.
    vi.stubGlobal(
      'fetch',
      vi.fn((url: string) =>
        String(url).includes('/api/i18n')
          ? Promise.reject(new Error('network down'))
          : Promise.resolve({ ok: true, json: async () => ({ plugins: [] }) } as Response),
      ),
    )
    await router.push('/')
    await router.isReady()
    const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
    await flushPromises()
    expect(w.find('router-view-stub').exists()).toBe(true)
    w.unmount()
  })

  it('does not hide the page again when the language changes', async () => {
    // `reload()` runs again on a language change. If the gate fell back, the
    // whole page would blank out every time the language is switched — a
    // regression that would only show up in use, which is why it is pinned
    // here.
    const w = await mountAt('/')
    expect(w.find('router-view-stub').exists()).toBe(true)
    const { reload } = useCatalog()
    const inFlight = reload()
    await Promise.resolve()
    expect(w.find('router-view-stub').exists()).toBe(true)
    await inFlight
    await flushPromises()
    expect(w.find('router-view-stub').exists()).toBe(true)
    w.unmount()
  })
})
