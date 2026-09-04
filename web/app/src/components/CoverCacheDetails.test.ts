import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const CATALOGUE = {
  cover_cache_open: 'Voir le détail du cache',
  cover_cache_title: 'Ce que le cache contient',
  cover_cache_hint: 'Relevé au moment de l’ouverture.',
  cover_cache_used: 'Occupé',
  cover_cache_entries: 'Entrées',
  cover_cache_entries_free: 'dont sans coût mémoire',
  cover_cache_renditions: 'Vignettes retenues',
  cover_cache_average: 'Poids moyen réel',
  cover_cache_stale: 'Vignettes périmées',
  cover_cache_belt: 'Plafond du nombre d’entrées',
  cover_cache_empty: 'Le cache est vide.',
  cover_cache_failed: 'Relevé indisponible.',
  reload: 'Recharger',
}

const SNAPSHOT = {
  used_bytes: 12_582_912,
  budget_bytes: 52_428_800,
  entries: 42,
  entries_free: 30,
  renditions: 12,
  renditions_bytes: 1_260_000,
  renditions_stale: 2,
  max_entries: 256,
}

async function mountPanel(payload: unknown = SNAPSHOT, status = 200) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOGUE), { status: 200 })
      if (url === '/api/cover-cache') {
        return new Response(JSON.stringify(payload), { status })
      }
      return new Response('unknown', { status: 404 })
    }),
  )
  const { useCatalog } = await import('../composables/useCatalog')
  const CoverCacheDetails = (await import('./CoverCacheDetails.vue')).default
  document.body.innerHTML = ''
  // The catalog is a module-level singleton that `ConfigView.vue` populates
  // on its own mount in production; mounted standalone here, nothing else
  // reloads it, so it is loaded explicitly before the panel opens -- the
  // component itself must not do this (see CoverCacheDetails.vue for why).
  await useCatalog().reload()
  const w = mount(CoverCacheDetails, { attachTo: document.body })
  await flushPromises()
  return w
}

describe('CoverCacheDetails', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
  })

  it('fetches nothing until the panel is opened', async () => {
    // **Loaded on opening, never polled.** The production change that would
    // break this: an `onMounted` that reads the snapshot, which would make
    // it read on every visit to the settings page, panel closed or not.
    const w = await mountPanel()
    const spy = globalThis.fetch as unknown as ReturnType<typeof vi.fn>
    expect(spy.mock.calls.filter((c) => c[0] === '/api/cover-cache')).toHaveLength(0)

    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => c[0] === '/api/cover-cache')).toHaveLength(1)
  })

  it('divides the total by the count to show the real average weight', async () => {
    // The line that matters: 1,260,000 / 12 = 105,000 bytes, i.e. 103 KiB.
    // This is the figure the owner checks against the predicted 98 KiB.
    const w = await mountPanel()
    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    // The dialog is mounted in a portal: it lives in document.body (same
    // convention as SystemView.test.ts and PlayerCard.test.ts).
    expect(document.body.querySelector('[data-cover-cache-average]')?.textContent).toContain('103')
  })

  it('says the cache is empty rather than dividing by zero', async () => {
    // `renditions: 0` would show `NaN` or `Infinity` to the user.
    const w = await mountPanel({ ...SNAPSHOT, renditions: 0, renditions_bytes: 0 })
    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    expect(document.body.querySelector('[data-cover-cache-average]')).toBeNull()
    expect(document.body.querySelector('[data-cover-cache-panel]')?.textContent).toContain('vide')
  })

  it('shows a message rather than an empty panel when the snapshot fails', async () => {
    // `api.get` **throws** on failure, unlike `api.put`. Without a `catch`,
    // opening the panel would produce an unhandled rejection and a mute
    // panel.
    const w = await mountPanel('boom', 500)
    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    expect(document.body.querySelector('[data-cover-cache-error]')).not.toBeNull()
  })

  it('re-reads on demand, and only then', async () => {
    const w = await mountPanel()
    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-cover-cache-reload]')!.click()
    await flushPromises()
    const spy = globalThis.fetch as unknown as ReturnType<typeof vi.fn>
    expect(spy.mock.calls.filter((c) => c[0] === '/api/cover-cache')).toHaveLength(2)
  })
})
