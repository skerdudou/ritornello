import { beforeEach, describe, expect, it, vi } from 'vitest'

// `useCatalog` partage un état de module (le catalogue) : chaque test repart
// d'un module frais pour ne step hériter de celui du test précédent.
describe('useCatalog', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
  })

  it('un échec transitoire garde le catalogue précédent, sans retomber sur les clés brutes', async () => {
    // Régression (revue 2026-07-27) : `.catch(() => ({}))` écrasait le
    // catalogue partagé par toutes les vues — un GET /api/i18n raté après un
    // changement de langue affichait `remote_title`, `status_title`… partout,
    // jusqu'à un rechargement manuel.
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
