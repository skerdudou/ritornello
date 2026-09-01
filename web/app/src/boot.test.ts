import { DEFAULT_MODE, DEFAULT_PRESET } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import { readBootTheme } from './boot'

describe('readBootTheme', () => {
  it('reads the choice injected by the core into the shell', () => {
    const win = { __RITORNELLO_THEME__: { theme: 'cyberpunk', mode: 'dark' } } as never
    expect(readBootTheme(win)).toEqual({ theme: 'cyberpunk', mode: 'dark' })
  })

  it('falls back to the defaults when nothing is injected', () => {
    expect(readBootTheme({} as never)).toEqual({ theme: DEFAULT_PRESET, mode: DEFAULT_MODE })
  })

  it('rejects an unknown mode rather than propagating it', () => {
    const win = { __RITORNELLO_THEME__: { theme: 'vercel', mode: 'system' } } as never
    expect(readBootTheme(win)).toEqual({ theme: 'vercel', mode: DEFAULT_MODE })
  })
})
