import type { Contract, ContractGap, ContractVersion, Refusal } from '../types'

/**
 * The words the page uses for a plugin's contracts, shared by the plugin table
 * (`ConfigView.vue`, a running plugin's badge) and the update dialog
 * (`UpdateDialog.vue`, what an offered plugin will be with the chosen core):
 * one wording for one verdict, wherever it is shown.
 *
 * Each branch names its key literally: a computed key would escape the check
 * that every key the page uses exists in every language (`i18nKeysUsed`).
 */
type Translate = (key: string, params?: Record<string, string | number>) => string

/** `1.1`: a contract version as the page writes it. */
export const fmt = (v: ContractVersion) => `${v.major}.${v.minor}`

/** A contract's name in the reader's language, never its wire word: the
 * same `plugin_kind_*` nouns the plugin table uses for a kind, plus
 * `plugin_kind_admin`. */
export function contractLabel(t: Translate, c: Contract): string {
  switch (c) {
    case 'source':
      return t('plugin_kind_source')
    case 'display':
      return t('plugin_kind_display')
    case 'input':
      return t('plugin_kind_input')
    case 'metadata':
      return t('plugin_kind_metadata')
    case 'admin':
      return t('plugin_kind_admin')
  }
}

/** The sentence a refusal reads, one per cause. Several gaps give several
 * sentences, joined. */
export function refusalText(t: Translate, r: Refusal): string {
  switch (r.reason) {
    case 'legacy':
      return t('plugin_incompatible_legacy', { found: r.found })
    case 'missing_contract':
      return t('plugin_incompatible_missing', { contract: contractLabel(t, r.contract) })
    case 'unexpected_contract':
      return t('plugin_incompatible_unexpected', { contract: contractLabel(t, r.contract) })
    case 'major':
      return r.gaps
        .map((g) =>
          t('plugin_incompatible_major', {
            contract: contractLabel(t, g.contract),
            found: fmt(g.plugin),
            expected: fmt(g.core),
          }),
        )
        .join(' · ')
  }
}

/** The sentence a running limited plugin's badge reads: one per limited
 * contract, joined. */
export function limitedText(t: Translate, gaps: ContractGap[]): string {
  return gaps
    .map((g) =>
      t('plugin_limited', { contract: contractLabel(t, g.contract), found: fmt(g.plugin), expected: fmt(g.core) }),
    )
    .join(' · ')
}
