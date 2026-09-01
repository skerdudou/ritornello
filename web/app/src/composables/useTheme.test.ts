import { presets, toast } from '@ritornello/ui'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { filterPresets, initTheme, useTheme } from './useTheme'

// Partial mock: we keep all the real exports of the kit (applyTheme, presets,
// api...) and replace only `toast.error` by a spy, to be able to check that
// the persistence failure is indeed reported without having to mount a real
// `<Toaster>`.
vi.mock('@ritornello/ui', async (importOriginal) => {
  const real = await importOriginal<typeof import('@ritornello/ui')>()
  return { ...real, toast: { ...real.toast, error: vi.fn() } }
})

function mockFetch() {
  const spy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
  vi.stubGlobal('fetch', spy)
  return spy
}

describe('useTheme', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    vi.mocked(toast.error).mockClear()
    document.documentElement.removeAttribute('style')
    document.documentElement.className = ''
    window.__RITORNELLO_THEME__ = { theme: 'northern-lights', mode: 'light' }
    initTheme()
  })

  it('starts from the injected choice and applies the variables', () => {
    const { theme, mode } = useTheme()
    expect(theme.value).toBe('northern-lights')
    expect(mode.value).toBe('light')
    expect(document.documentElement.style.getPropertyValue('--primary')).toBe('#34a85a')
  })

  it('toggleMode toggles the mode, applies and persists', async () => {
    const spy = mockFetch()
    const { mode, toggleMode } = useTheme()
    await toggleMode()
    expect(mode.value).toBe('dark')
    expect(document.documentElement.classList.contains('dark')).toBe(true)
    expect(spy).toHaveBeenCalledWith(
      '/api/theme',
      expect.objectContaining({
        method: 'PUT',
        body: JSON.stringify({ theme: 'northern-lights', mode: 'dark' }),
      }),
    )
    await toggleMode()
    expect(mode.value).toBe('light')
  })

  it('changing the preset keeps the current mode', async () => {
    mockFetch()
    const { set, theme, mode } = useTheme()
    await set({ mode: 'dark' })
    await set({ theme: 'vercel' })
    expect(theme.value).toBe('vercel')
    expect(mode.value).toBe('dark')
  })

  it('applies the choice locally even if persistence fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'boom' }), { status: 422 })),
    )
    const { set, theme } = useTheme()
    await set({ theme: 'cyberpunk' })
    // The choice stays visible: refusing to apply would give a frozen UI
    // without explanation.
    expect(theme.value).toBe('cyberpunk')
    // ... but the failure is reported: without this toast, the user would
    // never know that their choice will not survive a reload.
    expect(toast.error).toHaveBeenCalledWith('boom')
  })
})

describe('filterPresets', () => {
  it('without a filter, returns the 42 presets', () => {
    expect(filterPresets('')).toHaveLength(Object.keys(presets).length)
    expect(filterPresets('')).toHaveLength(42)
  })

  it('filters on the label, insensitive to case and whitespace', () => {
    const r = filterPresets('  NORTHERN ')
    expect(r).toHaveLength(1)
    expect(r[0]?.id).toBe('northern-lights')
  })

  it('also filters on the identifier', () => {
    expect(filterPresets('northern-lights')[0]?.label).toBe('Northern Lights')
  })

  it('returns an empty list on a filter without a match', () => {
    expect(filterPresets('zzzzz')).toEqual([])
  })
})
