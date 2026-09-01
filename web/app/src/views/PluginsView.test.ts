import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

async function mountView(plugins: object[]) {
  vi.resetModules()
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ plugins, active_source: 'radio' }), { status: 200 })))
  const { usePlugins } = await import('../composables/usePlugins')
  await usePlugins().refresh()
  const PluginsView = (await import('./PluginsView.vue')).default
  const router = createRouter({ history: createMemoryHistory(), routes: [{ path: '/plugins/:name/', component: { template: '<div />' } }, { path: '/', component: { template: '<div />' } }] })
  return mount(PluginsView, { global: { plugins: [router] } })
}

describe('PluginsView', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('lists the plugins with an admin page, in /api/status order, without duplicates', async () => {
    const w = await mountView([
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'mpd', kind: 'input', connected: true, admin: true },
      { name: 'mpd', kind: 'display', connected: true, admin: true },
      { name: 'console', kind: 'display', connected: true, admin: false },
    ])
    const links = w.findAll('[data-plugins-list] a')
    expect(links.map((a) => a.attributes('href'))).toEqual(['/plugins/files/', '/plugins/radio/', '/plugins/mpd/'])
  })

  it('says so when no plugin has a page', async () => {
    const w = await mountView([])
    expect(w.find('[data-plugins-empty]').exists()).toBe(true)
  })
})
