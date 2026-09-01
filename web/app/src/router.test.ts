import { describe, expect, it } from 'vitest'
import { router } from './router'

describe('router', () => {
  it('preserves historical URLs', async () => {
    await router.push('/')
    expect(router.currentRoute.value.name).toBe('home')
    await router.push('/config')
    expect(router.currentRoute.value.name).toBe('config')
    await router.push('/plugins/radio/')
    expect(router.currentRoute.value.name).toBe('plugin')
    expect(router.currentRoute.value.params.name).toBe('radio')
    // The canonical form does not move: it is an invariant pinned
    // elsewhere on the core side (`serves_shell("/plugins/radio/")`).
    expect(router.currentRoute.value.fullPath).toBe('/plugins/radio/')
  })

  it('redirects the old /status URL to /config', async () => {
    // The page was renamed (it configures more than it reports), but
    // /status has remained a valid URL since the server-side rendering
    // era: it now lands on the same page under its new name.
    await router.push('/status')
    expect(router.currentRoute.value.fullPath).toBe('/config')
    expect(router.currentRoute.value.name).toBe('config')
  })

  it('redirects the form without a trailing slash to the canonical form', async () => {
    // IMPORTANT 6 from the final review. `/plugins/radio` and
    // `/plugins/radio/` both matched the plugin route (the router is not
    // strict by default): the page mounted on the slash-less form, but its
    // modules then resolved `./api/data` to `/plugins/api/data` — which the
    // core interprets as the "api" plugin -> 404, empty table and every
    // button failing. The `base` prop removes the dependency on the URL's
    // shape; on top of that we canonicalize the URL so two forms never
    // coexist.
    await router.push('/plugins/generic-input')
    expect(router.currentRoute.value.fullPath).toBe('/plugins/generic-input/')
    expect(router.currentRoute.value.name).toBe('plugin')
    expect(router.currentRoute.value.params.name).toBe('generic-input')
  })

  it('/plugins/ is the plugin list, distinct from /plugins/<name>/', async () => {
    await router.push('/plugins/')
    expect(router.currentRoute.value.name).toBe('plugins')
    await router.push('/plugins/radio/')
    expect(router.currentRoute.value.name).toBe('plugin')
  })
})
