import { describe, expect, it } from 'vitest'
import { languageName } from './languages'

describe('languageName', () => {
  it('returns the name of the language in its own language, capitalized', () => {
    // Convention of language selectors, and the only one readable when one
    // does not understand the active language: finding "English" without
    // being able to read French.
    //
    // Capitalized systematically: typographic conventions diverge (English
    // capitalizes language names, French does not) and a list where the
    // entries alternate between the two reads badly. `Intl` renders
    // "français" in lower case, hence this normalisation.
    expect(languageName('fr')).toBe('Français')
    expect(languageName('en')).toBe('English')
    expect(languageName('de')).toBe('Deutsch')
    expect(languageName('es')).toBe('Español')
  })

  it('falls back on the code rather than disappearing from the selector', () => {
    // The codes come from the names of the `<lang>.toml` files: nothing
    // guarantees the browser knows them, and an entry without a label would
    // be worse than a code.
    expect(languageName('qqq')).toBe('qqq')
    expect(languageName('')).toBe('')
    expect(languageName('  ')).toBe('')
  })
})
