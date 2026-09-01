import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App.vue'
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
})
