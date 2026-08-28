import { presets, toast } from '@ritornello/ui'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { filterPresets, initTheme, useTheme } from './useTheme'

// Mock partiel : on garde tous les vrais exports du kit (applyTheme,
// presets, api...) et on remplace uniquement `toast.error` par un espion,
// pour pouvoir vérifier que l'échec de persistance est bien signalé sans
// avoir à monter un vrai `<Toaster>`.
vi.mock('@ritornello/ui', async (importOriginal) => {
  const reel = await importOriginal<typeof import('@ritornello/ui')>()
  return { ...reel, toast: { ...reel.toast, error: vi.fn() } }
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

  it('part du choix injecté et applique les variables', () => {
    const { theme, mode } = useTheme()
    expect(theme.value).toBe('northern-lights')
    expect(mode.value).toBe('light')
    expect(document.documentElement.style.getPropertyValue('--primary')).toBe('#34a85a')
  })

  it('toggleMode bascule le mode, applique et persiste', async () => {
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

  it('changer de preset conserve le mode courant', async () => {
    mockFetch()
    const { set, theme, mode } = useTheme()
    await set({ mode: 'dark' })
    await set({ theme: 'vercel' })
    expect(theme.value).toBe('vercel')
    expect(mode.value).toBe('dark')
  })

  it('applique le choix localement même si la persistance échoue', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'boum' }), { status: 422 })),
    )
    const { set, theme } = useTheme()
    await set({ theme: 'cyberpunk' })
    // Le choix reste visible : refuser d'appliquer donnerait une IHM figée
    // sans explication.
    expect(theme.value).toBe('cyberpunk')
    // ... mais l'échec est signalé : sans ce toast, l'utilisateur ne saurait
    // jamais que son choix ne survivra step au rechargement.
    expect(toast.error).toHaveBeenCalledWith('boum')
  })
})

describe('filterPresets', () => {
  it('sans filtre, renvoie les 42 presets', () => {
    expect(filterPresets('')).toHaveLength(Object.keys(presets).length)
    expect(filterPresets('')).toHaveLength(42)
  })

  it('filtre sur le libellé, insensible à la casse et aux espaces', () => {
    const r = filterPresets('  NORTHERN ')
    expect(r).toHaveLength(1)
    expect(r[0]?.id).toBe('northern-lights')
  })

  it('filtre aussi sur l’identifiant', () => {
    expect(filterPresets('northern-lights')[0]?.label).toBe('Northern Lights')
  })

  it('renvoie une list vide sur un filtre sans correspondance', () => {
    expect(filterPresets('zzzzz')).toEqual([])
  })
})
