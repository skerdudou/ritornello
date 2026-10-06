import type { ComponentOffer } from '../types'

/**
 * Why a row the core marked `installable: false` cannot be installed from
 * here, in words — chosen by **what the row is**, never one sentence for all.
 *
 * The core marks a row so for three different reasons, and only one of them
 * is "privileged": a privileged plugin of ours (`files`), whose packaging only
 * `ritornello-install` places; a third-party plugin whose last archive carried
 * more than its own binary (`archive_allowed`); and a language pack, ours or a
 * stranger's, whose last archive the pack reader refused (`install_pack`).
 * Telling the operator to use `ritornello-install` for a stranger's plugin or
 * a pack sends them to a tool that has nothing to do with either.
 *
 * Keyed on `third_party_repo` rather than on `kind === 'third_party'`: a
 * third-party **pack** has the kind `language_pack` and carries its source in
 * that same field (ruling P6).
 */
export function refusedNote(
  t: (key: string, params?: Record<string, string | number>) => string,
  c: ComponentOffer,
): string {
  if (c.kind === 'language_pack') return t('update_row_pack_refused')
  if (c.third_party_repo) return t('update_row_third_party_refused')
  return t('plugin_privileged_note')
}
