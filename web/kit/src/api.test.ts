import { beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from './api'

function mockFetch(response: Response) {
  const spy = vi.fn().mockResolvedValue(response)
  vi.stubGlobal('fetch', spy)
  return spy
}

describe('api', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('get renvoie le JSON décodé', async () => {
    mockFetch(new Response(JSON.stringify({ stations: [] }), { status: 200 }))
    await expect(api.get<{ stations: unknown[] }>('/x')).resolves.toEqual({ stations: [] })
  })

  it('get rejette sur un statut non ok', async () => {
    mockFetch(new Response('nope', { status: 502 }))
    await expect(api.get('/x')).rejects.toThrow('HTTP 502')
  })

  it('put renvoie null sur 204', async () => {
    const spy = mockFetch(new Response(null, { status: 204 }))
    await expect(api.put('/x', { a: 1 })).resolves.toBeNull()
    expect(spy).toHaveBeenCalledWith('/x', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ a: 1 }),
    })
  })

  it('put renvoie le message du champ error sur 422', async () => {
    mockFetch(new Response(JSON.stringify({ error: 'preset en double' }), { status: 422 }))
    await expect(api.put('/x', {})).resolves.toBe('preset en double')
  })

  it('put retombe sur HTTP <code> quand le corps n’est pas du JSON', async () => {
    mockFetch(new Response('plugin injoignable', { status: 502 }))
    await expect(api.put('/x', {})).resolves.toBe('HTTP 502')
  })

  it('post suit la même convention que put', async () => {
    const spy = mockFetch(new Response(null, { status: 204 }))
    await expect(api.post('/api/command', { cmd: 'VolumeUp' })).resolves.toBeNull()
    expect(spy.mock.calls[0]?.[1]).toMatchObject({ method: 'POST' })
  })

  it('put et post rendent une panne réseau comme message, jamais comme exception', async () => {
    // Régression (revue 2026-07-27) : un rejet de `fetch` lui-même (cœur en
    // cours de redémarrage, Wi-Fi coupé) sortait de la convention « valeur de
    // retour » — les appelants sans `try` laissaient l'utilisateur sans aucun
    // retour, avec une unhandled rejection en console pour seul indice.
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
    await expect(api.put('/x', {})).resolves.toBe('Failed to fetch')
    await expect(api.post('/x', {})).resolves.toBe('Failed to fetch')
  })
})
