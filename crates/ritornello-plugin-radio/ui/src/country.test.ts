import { describe, expect, it } from 'vitest'
import { countryName, displayableCountries, ALL_COUNTRIES } from './country'

const LIST = [
  { code: 'DE', stations: 6081 },
  { code: 'FR', stations: 2746 },
  { code: 'BE', stations: 300 },
  { code: 'US', stations: 7560 },
]

describe('countryName', () => {
  it('renders the countrys name in the requested language', () => {
    // This is what replaces a 241-country table to translate in every pack.
    expect(countryName('FR', 'fr')).toBe('France')
    expect(countryName('DE', 'fr')).toBe('Allemagne')
    expect(countryName('DE', 'en')).toBe('Germany')
    // Case and whitespace from the directory must not get in the way.
    expect(countryName(' be ', 'fr')).toBe('Belgique')
  })

  it('falls back to the code rather than disappearing', () => {
    // A code unknown to the engine must stay selectable: the directory
    // returns whatever it wants, and an entry with no label would be
    // worse than a code.
    //
    // `QQ` and not `ZZ`: the latter is a **valid** ISO code ("unknown
    // region"), which the engine translates — so it does not exercise the
    // fallback.
    expect(countryName('QQ', 'fr')).toBe('QQ')
    expect(countryName('', 'fr')).toBe('')
  })
})

describe('displayableCountries', () => {
  it('sorts by readable name, not by code', () => {
    // "Allemagne" is searched under the letter A, not DE.
    const names = displayableCountries(LIST, '', 'fr').map((p) => p.name)
    expect(names).toEqual(['Allemagne', 'Belgique', 'États-Unis', 'France'])
  })

  it('filters on the name, regardless of accents or case', () => {
    expect(displayableCountries(LIST, 'etats', 'fr').map((p) => p.code)).toEqual(['US'])
    expect(displayableCountries(LIST, 'ALLEM', 'fr').map((p) => p.code)).toEqual(['DE'])
    expect(displayableCountries(LIST, 'gi', 'fr').map((p) => p.code)).toEqual(['BE'])
  })

  it('also filters on the code, which is what one types when they know it', () => {
    expect(displayableCountries(LIST, 'fr', 'fr').map((p) => p.code)).toEqual(['FR'])
    expect(displayableCountries(LIST, 'us', 'fr').map((p) => p.code)).toEqual(['US'])
  })

  it('keeps the station count, which helps choose', () => {
    const fr = displayableCountries(LIST, 'france', 'fr')[0]
    expect(fr?.stations).toBe(2746)
  })

  it('renders an empty list when nothing matches', () => {
    expect(displayableCountries(LIST, 'zzzz', 'fr')).toEqual([])
    expect(displayableCountries([], '', 'fr')).toEqual([])
  })

  it('"all countries" is the empty string the plugin expects', () => {
    // The server contract is `country: ''`; any internal sentinel would
    // eventually leak into the request.
    expect(ALL_COUNTRIES).toBe('')
  })
})
