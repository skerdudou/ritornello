/**
 * Where a language pack comes from, in words: Ritornello's own pack says so,
 * a third party's names its repository (`owner/repo`, as the core stores it).
 * One function for the card and the add dialog, so they cannot word it twice.
 */
export function packSourceLabel(
  t: (key: string, params?: Record<string, string | number>) => string,
  source: string | null,
): string {
  return source === null ? t('language_pack_official') : t('language_pack_from', { source })
}
