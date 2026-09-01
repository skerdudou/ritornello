/**
 * Readable name of a language, in **its own language**: `fr` gives
 * "français", `en` gives "English".
 *
 * This is the convention of language selectors, and the only one that can be
 * read when one does not understand the active language: somebody landing on
 * a device set to French must be able to find "English" without being able to
 * read French.
 *
 * The names are not translated by our packs: the core only exposes codes (the
 * names of the `<lang>.toml` files), and `Intl.DisplayNames` renders them from
 * the browser's data — nothing to keep up to date on our side when a language
 * pack is added.
 *
 * Fallback on the code: a language unknown to the engine must stay
 * selectable, not disappear from the selector.
 */
export function languageName(code: string): string {
  const raw = code.trim()
  if (!raw) return ''
  try {
    const names = new Intl.DisplayNames([raw], { type: 'language' })
    const name = names.of(raw)
    // Capitalize only a **name**: when `Intl` does not know the code, it
    // returns it as is, and a code is displayed verbatim — "Qqq" would be
    // neither a name nor a code.
    return name && name !== raw ? capitalize(name, raw) : raw
  } catch {
    return raw
  }
}

/**
 * First letter capitalized.
 *
 * Typographic conventions diverge — English capitalizes language names
 * ("English"), French does not ("français") — and a list where the entries
 * alternate between the two reads badly. So we capitalize every entry.
 *
 * `toLocaleUpperCase` with the language concerned, and not `toUpperCase`: the
 * transformation depends on the language (Turkish distinguishes `i` and `ı`),
 * and it has no effect on scripts that have no case.
 */
function capitalize(name: string, language: string): string {
  const first = name.slice(0, 1).toLocaleUpperCase(language)
  return first + name.slice(1)
}
