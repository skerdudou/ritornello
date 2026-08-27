import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

async function monter(status: { plugins: { name: string; kind: string; connected: boolean; admin: boolean }[]; active_source: string }) {
  vi.resetModules()
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(status), { status: 200 })))
  const { useGreffons } = await import('../composables/useGreffons')
  await useGreffons().rafraichir()
  const NavBasse = (await import('./NavBasse.vue')).default
  const router = createRouter({ history: createMemoryHistory(), routes: [
    { path: '/', component: { template: '<div />' } },
    { path: '/plugins/', component: { template: '<div />' } },
    { path: '/plugins/:name/', component: { template: '<div />' } },
    { path: '/system', component: { template: '<div />' } },
    { path: '/config', component: { template: '<div />' } },
  ] })
  await router.push('/')
  await router.isReady()
  // Le routeur est renvoyé avec le wrapper : le test de la classe active sur
  // une navigation ultérieure (`/plugins/radio/`) en a besoin pour naviguer
  // après montage, sans quoi il n'aurait aucune prise sur l'instance montée.
  return { w: mount(NavBasse, { global: { plugins: [router] } }), router }
}

describe('NavBasse', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('quatre onglets, toujours, quel que soit le nombre de greffons', async () => {
    const { w } = await monter({ plugins: [
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'generic-input', kind: 'input', connected: true, admin: true },
    ], active_source: 'radio' })
    expect(w.findAll('[data-nav-basse] a')).toHaveLength(4)
    expect(w.get('[data-nav-greffons]').attributes('href')).toBe('/plugins/')
  })

  it('un seul greffon : l’onglet mène directement à sa page', async () => {
    const { w } = await monter({ plugins: [{ name: 'radio', kind: 'source', connected: true, admin: true }], active_source: 'radio' })
    expect(w.findAll('[data-nav-basse] a')).toHaveLength(4)
    expect(w.get('[data-nav-greffons]').attributes('href')).toBe('/plugins/radio/')
  })

  it('aucun greffon à page : l’onglet mène à la liste, qui dira qu’elle est vide', async () => {
    const { w } = await monter({ plugins: [], active_source: '' })
    expect(w.findAll('[data-nav-basse] a')).toHaveLength(4)
    expect(w.get('[data-nav-greffons]').attributes('href')).toBe('/plugins/')
  })

  it('l’onglet Greffons reste allumé sur la page d’un greffon, routes sœurs de /plugins/', async () => {
    const { w, router } = await monter({ plugins: [
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'generic-input', kind: 'input', connected: true, admin: true },
    ], active_source: 'radio' })
    await router.push('/plugins/radio/')
    await flushPromises()
    expect(w.get('[data-nav-greffons]').classes()).toContain('text-primary')
    await router.push('/')
    await flushPromises()
    expect(w.get('[data-nav-greffons]').classes()).not.toContain('text-primary')
  })
})
