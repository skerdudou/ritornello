// Directory country: pure logic, no Vue and no network, so testable as-is.
//
// The plugin only carries **ISO codes** (see `DirectoryCountry`): the
// readable name is rendered here by `Intl.DisplayNames`, so in the
// browser's language and with no 241-country table to keep up to date or
// to translate on our side. This also avoids the bug the previous version
// had, where the displayed label came from a translation key resolved too
// early.

export interface Country {
  code: string
  stations: number
}

/** A country ready to display: code, readable name, station count. */
export interface DisplayableCountry extends Country {
  name: string
}

/** Code for the "all countries" choice: this is what the plugin expects (`country: ''`). */
export const ALL_COUNTRIES = ''

/**
 * Language to use for country names.
 *
 * The browser's, not the device's: the catalog passed to plugin UIs does
 * not carry the language code, and adding it to the contract for this sole
 * use would be disproportionate. Accepted consequence: a browser set to
 * English will display "Germany" on a device set to French.
 */
function browserLanguage(): string {
  return (typeof navigator !== 'undefined' && navigator.language) || 'en'
}

/**
 * Readable name of an ISO code. Falls back to the code itself: a code
 * unknown to the engine (or an engine with no `Intl.DisplayNames`) must
 * stay selectable, not disappear from the list.
 */
export function countryName(code: string, language: string = browserLanguage()): string {
  const raw = code.trim().toUpperCase()
  if (!raw) return ''
  try {
    const names = new Intl.DisplayNames([language], { type: 'region' })
    return names.of(raw) ?? raw
  } catch {
    return raw
  }
}

/** Strips accents and case, so that "etats" finds "États-Unis". */
function fold(s: string): string {
  return s
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
}

/**
 * List to display: filtered on the name **or** the code, then sorted by
 * name.
 *
 * The sort is done on the readable name, not the code: "Allemagne" is
 * searched under the letter A, not DE. The filter also accepts the code,
 * because that's what one types when they know it.
 */
export function displayableCountries(
  list: Country[],
  filter = '',
  language: string = browserLanguage(),
): DisplayableCountry[] {
  const f = fold(filter.trim())
  return list
    .map((p) => ({ ...p, name: countryName(p.code, language) }))
    .filter((p) => !f || fold(p.name).includes(f) || fold(p.code).includes(f))
    .sort((a, b) => a.name.localeCompare(b.name, language))
}
