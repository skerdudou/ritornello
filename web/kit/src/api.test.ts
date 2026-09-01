import { beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from './api'

function mockFetch(response: Response) {
  const spy = vi.fn().mockResolvedValue(response)
  vi.stubGlobal('fetch', spy)
  return spy
}

describe('api', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('get returns the decoded JSON', async () => {
    mockFetch(new Response(JSON.stringify({ stations: [] }), { status: 200 }))
    await expect(api.get<{ stations: unknown[] }>('/x')).resolves.toEqual({ stations: [] })
  })

  it('get rejects on a non-ok status', async () => {
    mockFetch(new Response('nope', { status: 502 }))
    await expect(api.get('/x')).rejects.toThrow('HTTP 502')
  })

  it('get surfaces the cause carried by the body, not the bare code', async () => {
    // Measured: the core puts its cause in the body of a 502, but only `send`
    // read it. Loading a plugin page goes through `get`, and showed "HTTP 502"
    // where the same failure on a PUT said why.
    mockFetch(
      new Response(JSON.stringify({ error: 'the plugin took more than 5 s to answer' }), {
        status: 502,
      }),
    )
    await expect(api.get('/x')).rejects.toThrow('more than 5 s')
  })

  it('put returns null on 204', async () => {
    const spy = mockFetch(new Response(null, { status: 204 }))
    await expect(api.put('/x', { a: 1 })).resolves.toBeNull()
    expect(spy).toHaveBeenCalledWith('/x', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ a: 1 }),
    })
  })

  it('put returns the message of the error field on 422', async () => {
    mockFetch(new Response(JSON.stringify({ error: 'duplicate preset' }), { status: 422 }))
    await expect(api.put('/x', {})).resolves.toBe('duplicate preset')
  })

  it('put falls back to HTTP <code> when the body is not JSON', async () => {
    mockFetch(new Response('plugin unreachable', { status: 502 }))
    await expect(api.put('/x', {})).resolves.toBe('HTTP 502')
  })

  it('post follows the same convention as put', async () => {
    const spy = mockFetch(new Response(null, { status: 204 }))
    await expect(api.post('/api/command', { cmd: 'VolumeUp' })).resolves.toBeNull()
    expect(spy.mock.calls[0]?.[1]).toMatchObject({ method: 'POST' })
  })

  it('put and post render a network failure as a message, never as an exception', async () => {
    // Regression (review 2026-07-27): a rejection of `fetch` itself (core
    // restarting, Wi-Fi down) escaped the "return value" convention — callers
    // without a `try` left the user with no feedback at all, with an unhandled
    // rejection in the console as the only clue.
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
    await expect(api.put('/x', {})).resolves.toBe('Failed to fetch')
    await expect(api.post('/x', {})).resolves.toBe('Failed to fetch')
  })
})
