import { DEFAULT_MODE, DEFAULT_PRESET } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import { readBootTheme } from './boot'

describe('readBootTheme', () => {
  it('lit le choix injecté par le cœur dans le shell', () => {
    const win = { __RITORNELLO_THEME__: { theme: 'cyberpunk', mode: 'dark' } } as never
    expect(readBootTheme(win)).toEqual({ theme: 'cyberpunk', mode: 'dark' })
  })

  it('retombe sur les défauts quand rien n’est injecté', () => {
    expect(readBootTheme({} as never)).toEqual({ theme: DEFAULT_PRESET, mode: DEFAULT_MODE })
  })

  it('rejette un mode inconnu plutôt que de le propager', () => {
    const win = { __RITORNELLO_THEME__: { theme: 'vercel', mode: 'system' } } as never
    expect(readBootTheme(win)).toEqual({ theme: 'vercel', mode: DEFAULT_MODE })
  })
})
