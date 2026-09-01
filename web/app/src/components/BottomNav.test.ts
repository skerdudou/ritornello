import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

async function mountWith(status: { plugins: { name: string; kind: string; connected: boolean; admin: boolean }[]; active_source: string }) {
  vi.resetModules()
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(status), { status: 200 })))
  const { usePlugins } = await import('../composables/usePlugins')
  await usePlugins().refresh()
  const BottomNav = (await import('./BottomNav.vue')).default
  const router = createRouter({ history: createMemoryHistory(), routes: [
    { path: '/', component: { template: '<div />' } },
    { path: '/plugins/', component: { template: '<div />' } },
    { path: '/plugins/:name/', component: { template: '<div />' } },
    { path: '/system', component: { template: '<div />' } },
    { path: '/config', component: { template: '<div />' } },
  ] })
  await router.push('/')
  await router.isReady()
  // The router is returned along with the wrapper: the active-class test on a
  // later navigation (`/plugins/radio/`) needs it to navigate after mounting,
  // otherwise it would have no handle on the mounted instance.
  return { w: mount(BottomNav, { global: { plugins: [router] } }), router }
}

describe('BottomNav', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('four tabs, always, whatever the number of plugins', async () => {
    const { w } = await mountWith({ plugins: [
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'generic-input', kind: 'input', connected: true, admin: true },
    ], active_source: 'radio' })
    expect(w.findAll('[data-bottom-nav] a')).toHaveLength(4)
    expect(w.get('[data-nav-plugins]').attributes('href')).toBe('/plugins/')
  })

  it('a single plugin: the tab leads directly to its page', async () => {
    const { w } = await mountWith({ plugins: [{ name: 'radio', kind: 'source', connected: true, admin: true }], active_source: 'radio' })
    expect(w.findAll('[data-bottom-nav] a')).toHaveLength(4)
    expect(w.get('[data-nav-plugins]').attributes('href')).toBe('/plugins/radio/')
  })

  it('no plugin with a page: the tab leads to the list, which will say it is empty', async () => {
    const { w } = await mountWith({ plugins: [], active_source: '' })
    expect(w.findAll('[data-bottom-nav] a')).toHaveLength(4)
    expect(w.get('[data-nav-plugins]').attributes('href')).toBe('/plugins/')
  })

  it('the Plugins tab stays lit on a plugin page, sibling routes of /plugins/', async () => {
    const { w, router } = await mountWith({ plugins: [
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'generic-input', kind: 'input', connected: true, admin: true },
    ], active_source: 'radio' })
    await router.push('/plugins/radio/')
    await flushPromises()
    expect(w.get('[data-nav-plugins]').classes()).toContain('text-primary')
    await router.push('/')
    await flushPromises()
    expect(w.get('[data-nav-plugins]').classes()).not.toContain('text-primary')
  })
})
