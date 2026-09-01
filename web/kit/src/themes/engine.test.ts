import { beforeEach, describe, expect, it } from 'vitest'
import {
  applyTheme,
  DEFAULT_MODE,
  DEFAULT_PRESET,
  fontFamilies,
  presets,
  resolveVars,
  withFallback,
} from './engine'

describe('presets', () => {
  it('embeds the 42 upstream presets, including the default', () => {
    expect(Object.keys(presets)).toHaveLength(42)
    expect(presets[DEFAULT_PRESET]?.label).toBe('Northern Lights')
    expect(DEFAULT_MODE).toBe('light')
  })

  it('every preset has a label and its two modes', () => {
    for (const [name, p] of Object.entries(presets)) {
      expect(p.label, name).toBeTruthy()
      expect(p.styles.light.background, name).toBeTruthy()
      expect(p.styles.dark.background, name).toBeTruthy()
    }
  })
})

describe('resolveVars', () => {
  it('overlays the mode block on the light block', () => {
    const preset = {
      label: 'T',
      styles: { light: { background: '#fff', radius: '0.5rem' }, dark: { background: '#000' } },
    }
    const vars = resolveVars(preset, 'dark')
    expect(vars.background).toBe('#000')
    // `radius` is not redefined by the dark block: it comes from the light block.
    expect(vars.radius).toBe('0.5rem')
  })

  it('in light mode, the dark block is ignored', () => {
    const preset = {
      label: 'T',
      styles: { light: { background: '#fff' }, dark: { background: '#000' } },
    }
    expect(resolveVars(preset, 'light').background).toBe('#fff')
  })
})

describe('applyTheme', () => {
  let root: HTMLElement

  beforeEach(() => {
    document.head.innerHTML = ''
    root = document.createElement('div')
  })

  it('writes each key of the preset as a CSS variable', () => {
    applyTheme(DEFAULT_PRESET, 'light', root)
    expect(root.style.getPropertyValue('--background')).toBe('#f9f9fa')
    expect(root.style.getPropertyValue('--primary')).toBe('#34a85a')
    expect(root.style.getPropertyValue('--radius')).toBe('0.5rem')
  })

  it('applies the dark block in dark mode and sets the `dark` class', () => {
    applyTheme(DEFAULT_PRESET, 'dark', root)
    expect(root.style.getPropertyValue('--background')).toBe('#1a1d23')
    expect(root.classList.contains('dark')).toBe(true)
    applyTheme(DEFAULT_PRESET, 'light', root)
    expect(root.classList.contains('dark')).toBe(false)
  })

  it('applies an unknown key without complaint (generic iteration)', () => {
    // No hard-coded list of keys: an upstream preset gaining a variable must
    // work without touching the code.
    const root2 = document.createElement('div')
    applyTheme('__test__', 'light', root2, document, {
      __test__: { label: 'T', styles: { light: { 'brand-new-variable': '#123456' }, dark: {} } },
    })
    expect(root2.style.getPropertyValue('--brand-new-variable')).toBe('#123456')
  })

  it('purges the variables of the previous theme', () => {
    applyTheme('__a__', 'light', root, document, {
      __a__: { label: 'A', styles: { light: { 'only-in-a': '#111' }, dark: {} } },
    })
    expect(root.style.getPropertyValue('--only-in-a')).toBe('#111')
    applyTheme('__b__', 'light', root, document, {
      __b__: { label: 'B', styles: { light: { background: '#222' }, dark: {} } },
    })
    expect(root.style.getPropertyValue('--only-in-a')).toBe('')
  })

  it('purges each root by its own keys, with no leak between roots', () => {
    // Regression: `applied` was a single module state, shared by all roots. A
    // call on one root interleaved between two calls on another root
    // overwrote the latter's memory of keys: on its next call, it purged the
    // other root's keys (with no effect, they are not there) instead of its
    // own — a variable set by its own previous call then stayed stale on it.
    // Indexing by root (`WeakMap`) removes this coupling.
    const rootA = document.createElement('div')
    const rootB = document.createElement('div')

    // 1) rootA receives a variable it will not redefine afterwards.
    applyTheme('__a1__', 'light', rootA, document, {
      __a1__: { label: 'A1', styles: { light: { 'only-in-a1': '#111' }, dark: {} } },
    })
    // 2) rootB receives a theme in between: with a global state, this is the
    // call that would overwrite rootA's memory of keys.
    applyTheme('__b__', 'light', rootB, document, {
      __b__: { label: 'B', styles: { light: { 'only-in-b': '#222' }, dark: {} } },
    })
    // 3) rootA receives a second theme that does not redefine
    // `only-in-a1`: it must be purged, based on the keys *rootA* had
    // itself set (not rootB's).
    applyTheme('__a2__', 'light', rootA, document, {
      __a2__: { label: 'A2', styles: { light: { 'only-in-a2': '#333' }, dark: {} } },
    })

    expect(rootA.style.getPropertyValue('--only-in-a1')).toBe('')
    expect(rootA.style.getPropertyValue('--only-in-a2')).toBe('#333')
    // `rootB` inherited nothing from rootA's keys and keeps its own.
    expect(rootB.style.getPropertyValue('--only-in-a1')).toBe('')
    expect(rootB.style.getPropertyValue('--only-in-a2')).toBe('')
    expect(rootB.style.getPropertyValue('--only-in-b')).toBe('#222')
  })

  it('ignores an unknown preset id without throwing', () => {
    applyTheme('preset-that-does-not-exist', 'light', root)
    expect(root.style.getPropertyValue('--background')).toBe('')
  })

  it('injects a single font link and replaces it on change', () => {
    applyTheme(DEFAULT_PRESET, 'light', root)
    const links = () => [...document.head.querySelectorAll('link[data-ritornello-fonts]')]
    expect(links()).toHaveLength(1)
    expect(links()[0]?.getAttribute('href')).toContain('Plus+Jakarta+Sans')
    applyTheme('vercel', 'light', root)
    expect(links()).toHaveLength(1)
  })
})

describe('fonts', () => {
  it('extracts the cited families, without duplicates', () => {
    const families = fontFamilies({
      'font-sans': 'Plus Jakarta Sans, sans-serif',
      'font-mono': 'JetBrains Mono, monospace',
      'font-serif': 'Plus Jakarta Sans, serif',
      background: '#fff',
    })
    expect(families).toEqual(['Plus Jakarta Sans', 'JetBrains Mono'])
  })

  it('does not keep generic families on their own', () => {
    expect(fontFamilies({ 'font-sans': 'system-ui, sans-serif' })).toEqual([])
  })

  it('adds a system fallback to every font stack', () => {
    expect(withFallback('font-sans', 'Plus Jakarta Sans')).toBe(
      'Plus Jakarta Sans, system-ui, sans-serif',
    )
    expect(withFallback('font-mono', 'JetBrains Mono')).toBe('JetBrains Mono, ui-monospace, monospace')
    // Fallback already present: not duplicated.
    expect(withFallback('font-sans', 'Inter, sans-serif')).toBe('Inter, sans-serif')
    // Non-typographic key: value unchanged.
    expect(withFallback('background', '#fff')).toBe('#fff')
  })
})
