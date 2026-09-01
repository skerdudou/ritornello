import { afterEach, describe, expect, it, vi } from 'vitest'
import { usePresets } from './usePresets'

describe('usePresets', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('names a preset by source and number', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      sources: [
        { name: 'radio', presets: [{ index: 1, name: 'FIP' }, { index: 2, name: 'France Inter' }] },
        { name: 'cd' },
      ],
    }), { status: 200 })))
    const { reload, nameOf } = usePresets()
    expect(nameOf('radio', 1)).toBeNull()
    await reload()
    expect(nameOf('radio', 1)).toBe('FIP')
    expect(nameOf('radio', 2)).toBe('France Inter')
    // A source without a list (the cd): numbers only, as today.
    expect(nameOf('cd', 1)).toBeNull()
    expect(nameOf('radio', 9)).toBeNull()
  })

  it('an unreachable core leaves the previous list', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ sources: [{ name: 'radio', presets: [{ index: 1, name: 'FIP' }] }] }), { status: 200 }))
      .mockRejectedValueOnce(new Error('network'))
    vi.stubGlobal('fetch', fetch)
    const { reload, nameOf } = usePresets()
    await reload()
    await reload()
    expect(nameOf('radio', 1)).toBe('FIP')
  })

  it('a response without `sources` leaves the previous list', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ sources: [{ name: 'radio', presets: [{ index: 1, name: 'FIP' }] }] }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 }))
    vi.stubGlobal('fetch', fetch)
    const { reload, nameOf } = usePresets()
    await reload()
    await reload()
    expect(nameOf('radio', 1)).toBe('FIP')
  })
})
