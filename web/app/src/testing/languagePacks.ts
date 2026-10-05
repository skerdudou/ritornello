import type { LanguagePackRow } from '../types'

/**
 * A `/api/locale` pack row as the core serves it for a language whose only
 * pack is Ritornello's own: `packs` holds that one pack, `update_available`
 * follows the two versions, and there is no overlap. Tests that care about a
 * third-party pack or an overlap pass `extra`.
 */
export function packRow(
  language: string,
  installed: string | null,
  offered: string | null,
  extra: Partial<LanguagePackRow> = {},
): LanguagePackRow {
  return {
    language,
    installed,
    offered,
    update_available: installed !== null && offered !== null && installed !== offered,
    packs: [{ id: `ritornello-lang-${language}`, source: null, installed, offered }],
    overlaps: [],
    ...extra,
  }
}
