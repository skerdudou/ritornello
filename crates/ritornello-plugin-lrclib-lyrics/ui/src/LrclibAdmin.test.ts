import { toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import LrclibAdmin from './LrclibAdmin.vue'

// Keep the real kit and replace only the toast entry this view uses, so it
// can be observed without showing a notification.
vi.mock('@ritornello/ui', async () => {
  const real = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...real, toast: { ...real.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOG = {
  title: 'Lyrics cache',
  intro: 'Lyrics found on LRCLIB are kept on this device.',
  size: 'Size on disk',
  found: 'Songs with lyrics',
  absent: 'Songs without lyrics',
  size_bytes: '{n} B',
  size_kib: '{n} KiB',
  size_mib: '{n} MiB',
  clear: 'Clear',
  attribution: 'Lyrics come from LRCLIB, a free and open lyrics database:',
  bad_request: 'Unexpected request: {detail}',
  clear_failed: 'could not empty the lyrics cache',
  // From the common vocabulary, merged into every plugin catalog.
  loading: 'Loading…',
}

const BASE = '/plugins/lrclib-lyrics/'

/** Mounts the view with a spied `fetch`: each GET answers the next of
 *  `answers` (the last one repeats), PUTs are logged and answer 204 unless
 *  `putResponse` says otherwise. */
async function mountView(
  answers: Array<{ bytes: number; found: number; absent: number }>,
  putResponse?: () => Response,
) {
  const puts: Array<{ url: string; body: unknown }> = []
  const gets: string[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        puts.push({ url, body: JSON.parse(String(init.body)) })
        return putResponse ? putResponse() : new Response(null, { status: 204 })
      }
      const answer = answers[Math.min(gets.length, answers.length - 1)]
      gets.push(url)
      return new Response(JSON.stringify(answer), { status: 200 })
    }),
  )
  const w = mount(LrclibAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, puts, gets }
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
})

describe('LrclibAdmin', () => {
  it('shows the cache size and counts', async () => {
    const { w, gets } = await mountView([{ bytes: 3 * 1024 * 1024 + 512 * 1024, found: 412, absent: 37 }])
    expect(gets).toEqual(['/plugins/lrclib-lyrics/api/data'])
    expect(w.get('[data-lyrics-cache-size]').text()).toBe('3.5 MiB')
    expect(w.get('[data-lyrics-cache-found]').text()).toBe('412')
    expect(w.get('[data-lyrics-cache-absent]').text()).toBe('37')
  })

  it('picks the unit that keeps the size readable', async () => {
    for (const [bytes, shown] of [
      [0, '0 B'],
      [1023, '1023 B'],
      [1536, '1.5 KiB'],
      [20 * 1024 * 1024, '20.0 MiB'],
    ] as const) {
      const { w } = await mountView([{ bytes, found: 0, absent: 0 }])
      expect(w.get('[data-lyrics-cache-size]').text()).toBe(shown)
    }
  })

  it('clearing sends the clear action and reloads', async () => {
    const { w, puts, gets } = await mountView([
      { bytes: 2048, found: 3, absent: 1 },
      { bytes: 0, found: 0, absent: 0 },
    ])
    await w.get('[data-lyrics-cache-clear]').trigger('click')
    await flushPromises()

    expect(puts).toEqual([{ url: '/plugins/lrclib-lyrics/api/data', body: { action: 'clear' } }])
    expect(gets).toHaveLength(2)
    expect(w.get('[data-lyrics-cache-found]').text()).toBe('0')
    expect(w.get('[data-lyrics-cache-size]').text()).toBe('0 B')
  })

  it('a refused clear shows the server sentence and still reloads', async () => {
    const serverMessage = 'could not empty the lyrics cache'
    const { w, gets } = await mountView([{ bytes: 2048, found: 3, absent: 1 }], () =>
      new Response(JSON.stringify({ error: serverMessage }), { status: 422 }),
    )
    await w.get('[data-lyrics-cache-clear]').trigger('click')
    await flushPromises()

    expect(toast.error).toHaveBeenCalledWith(serverMessage)
    expect(gets).toHaveLength(2)
  })

  it('credits LRCLIB with a link to its site', async () => {
    const { w } = await mountView([{ bytes: 0, found: 0, absent: 0 }])
    expect(w.get('a[href="https://lrclib.net"]').attributes('rel')).toContain('noopener')
  })
})
