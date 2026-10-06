/**
 * Where a language pack comes from, in words: Ritornello's own pack says so,
 * a third party's names its repository (`owner/repo`, as the core stores it).
 * One function for the card and the add dialog, so they cannot word it twice.
 */
/**
 * The language a pack id names: ours, `ritornello-lang-<language>`, or a
 * third party's, `ritornello-xlang-<language>-<12 hex digits>` (the shapes
 * `langpack::store::pack_id` and `third_party_pack_id` form), or `null` for
 * anything else. A third party's id is a digest, never something to show an
 * operator: this is what a row names instead.
 */
export function packLanguage(id: string): string | null {
  const theirs = /^ritornello-xlang-(.+)-[0-9a-f]{12}$/.exec(id)
  if (theirs) return theirs[1]!
  const ours = /^ritornello-lang-(.+)$/.exec(id)
  return ours ? ours[1]! : null
}

export function packSourceLabel(
  t: (key: string, params?: Record<string, string | number>) => string,
  source: string | null,
): string {
  return source === null ? t('language_pack_official') : t('language_pack_from', { source })
}
