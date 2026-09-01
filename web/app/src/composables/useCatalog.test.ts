import { beforeEach, describe, expect, it, vi } from 'vitest'

// `useCatalog` shares module state (the catalog): each test starts over from
// a fresh module so as not to inherit the one of the previous test.
describe('useCatalog', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
  })

  it('a transient failure keeps the previous catalog, without falling back to raw keys', async () => {
    // Regression (review 2026-07-27): `.catch(() => ({}))` overwrote the
    // catalog shared by all the views — a failed GET /api/i18n after a
    // language change displayed `remote_title`, `status_title`... everywhere,
    // until a manual reload.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ remote_title: 'Télécommande' }), { status: 200 }),
      )
      .mockRejectedValueOnce(new TypeError('Failed to fetch'))
    vi.stubGlobal('fetch', spy)
    const { useCatalog } = await import('./useCatalog')
    const { t, reload } = useCatalog()
    await reload()
    expect(t.value('remote_title')).toBe('Télécommande')
    await reload()
    expect(t.value('remote_title')).toBe('Télécommande')
    expect(spy).toHaveBeenCalledTimes(2)
  })
})
