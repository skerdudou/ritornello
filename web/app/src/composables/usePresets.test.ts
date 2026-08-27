import { afterEach, describe, expect, it, vi } from 'vitest'
import { usePresets } from './usePresets'

describe('usePresets', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('nomme une présélection par source et numéro', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      sources: [
        { name: 'radio', presets: [{ index: 1, name: 'FIP' }, { index: 2, name: 'France Inter' }] },
        { name: 'cd' },
      ],
    }), { status: 200 })))
    const { recharger, nomDe } = usePresets()
    expect(nomDe('radio', 1)).toBeNull()
    await recharger()
    expect(nomDe('radio', 1)).toBe('FIP')
    expect(nomDe('radio', 2)).toBe('France Inter')
    // Une source sans liste (le cd) : numéros seuls, comme aujourd'hui.
    expect(nomDe('cd', 1)).toBeNull()
    expect(nomDe('radio', 9)).toBeNull()
  })

  it('un cœur injoignable laisse la liste précédente', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ sources: [{ name: 'radio', presets: [{ index: 1, name: 'FIP' }] }] }), { status: 200 }))
      .mockRejectedValueOnce(new Error('réseau'))
    vi.stubGlobal('fetch', fetch)
    const { recharger, nomDe } = usePresets()
    await recharger()
    await recharger()
    expect(nomDe('radio', 1)).toBe('FIP')
  })

  it('une réponse sans `sources` laisse la liste précédente', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ sources: [{ name: 'radio', presets: [{ index: 1, name: 'FIP' }] }] }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 }))
    vi.stubGlobal('fetch', fetch)
    const { recharger, nomDe } = usePresets()
    await recharger()
    await recharger()
    expect(nomDe('radio', 1)).toBe('FIP')
  })
})
