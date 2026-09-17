import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { ComponentOffer, UpdatePayload } from '../types'
import UpdateSummary from './UpdateSummary.vue'

// A literal copy of the relevant French strings from `deploy/locales/core/fr.toml`
// rather than an invented fixture: the assertions below check the exact
// sentence a French appliance shows, punctuation included (the French pack
// puts a space before the colon in `update_out_of_step_one`, the English one
// does not — a fixture that made up its own text could hide that difference).
// This drifted once already (m8): `update_only_prereleases` quoted a label
// the pack no longer uses. Keep it copy-pasted from `fr.toml`, not retyped.
const CATALOG = {
  update_out_of_step: '{count} composants à mettre à jour vers {version}',
  update_out_of_step_one: '{name} : {installed} → {offered}',
  update_aligned: 'À jour',
  update_no_release: "Aucune version publiée pour l'instant",
  update_only_prereleases:
    'Seules des préversions sont publiées ; cochez « Proposer les versions beta » ci-dessous pour qu\'elles vous soient proposées',
  update_never_checked: 'Jamais vérifié',
  update_last_attempt_failed: 'La dernière tentative a échoué',
  update_detail: 'Détail',
}

// Same priming as `UpdateCard.test.ts`: the shared catalog singleton has to
// answer before a mount, or every label renders as its raw key.
beforeEach(async () => {
  resetCatalog()
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) =>
      url === '/api/i18n'
        ? new Response(JSON.stringify(CATALOG), { status: 200 })
        : new Response('', { status: 404 }),
    ),
  )
  await useCatalog().reload()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

function component(name: string, installed: string, offered: string): ComponentOffer {
  return {
    name,
    kind: 'plugin',
    declared: true,
    binary_present: true,
    installed,
    offered,
    availability: 'update_available',
  }
}

/**
 * Builds only what each test states: how many components are out of step (or
 * the one-component shortcut), the release they point to, and the outcome.
 * `UpdateCard.test.ts` already owns the field-by-field builder used to prove
 * swap-safety on a single row; this one is about counting and folding, so it
 * defaults to two out-of-step rows — one is not enough to exercise the count
 * sentence at all (it would take the singular branch instead), and neither
 * word appears without a component actually out of step.
 */
function payload(
  over: {
    outOfStep?: number
    release?: string
    only?: 'core'
    from?: string
    to?: string
    outcome?: UpdatePayload['outcome']
    /** Overrides the generated rows entirely, for a test that needs rows it
     *  can tell apart (m9: two identical rows cannot prove an order). */
    components?: ComponentOffer[]
  } = {},
): UpdatePayload {
  const components: ComponentOffer[] =
    over.components ??
    (over.only === 'core'
      // The core's row is always named literally "core" (`state.rs`'s
      // `component_offers`), never the French "cœur" — this fixture used to
      // invent that name (m8), a mismatch the assertion below could not
      // catch because it only checked the numbers either side of it.
      ? [{ ...component('core', over.from!, over.to!), kind: 'core' }]
      : Array.from({ length: over.outOfStep ?? 2 }, (_, i) =>
          component(`plugin-${i}`, '0.2.0', '0.2.0-beta.1'),
        ))
  return {
    outcome: over.outcome ?? { kind: 'ok' },
    release_version: over.release ?? '0.2.0-beta.1',
    release_url: null,
    last_check_unix_s: 1_760_000_000,
    components,
    busy: null,
    last_rollback: null,
  }
}

describe('UpdateSummary', () => {
  it('counts the components out of step and names the offered version', () => {
    const w = mount(UpdateSummary, { props: { update: payload({ outOfStep: 11, release: '0.2.0-beta.1' }) } })
    expect(w.find('[data-update-summary]').text()).toBe('11 composants à mettre à jour vers 0.2.0-beta.1')
  })

  it('names the single component instead of counting to one', () => {
    // A count of one is less useful than the name it hides.
    const w = mount(UpdateSummary, { props: { update: payload({ only: 'core', from: '0.2.0', to: '0.2.0-beta.1' }) } })
    expect(w.find('[data-update-summary]').text()).toBe('core : 0.2.0 → 0.2.0-beta.1')
  })

  it('says up to date when nothing is out of step', () => {
    const w = mount(UpdateSummary, { props: { update: payload({ outOfStep: 0 }) } })
    expect(w.find('[data-update-summary]').text()).toBe('À jour')
  })

  it('keeps the sentence of an outcome that counts nothing', () => {
    // Never checked, nothing published, only prereleases, last check failed:
    // each rebuilt every row against an empty offer, so a count would be a
    // claim about a device that has not looked. Positive assertions
    // (`toBe`), not `not.toContain('composants')`: the weaker form let
    // `failed` fall through to "À jour" — itself the word "composants"
    // never appears in either sentence — and pass unnoticed (Major D).
    const cases: Array<[UpdatePayload['outcome'], string]> = [
      [{ kind: 'never_checked' }, CATALOG.update_never_checked],
      [{ kind: 'no_release' }, CATALOG.update_no_release],
      [{ kind: 'only_prereleases' }, CATALOG.update_only_prereleases],
      [{ kind: 'failed', detail: 'GitHub unreachable' }, CATALOG.update_last_attempt_failed],
    ]
    for (const [outcome, expected] of cases) {
      const w = mount(UpdateSummary, { props: { update: payload({ outcome }) } })
      expect(w.find('[data-update-summary]').text()).toBe(expected)
    }
  })

  // N1: `CheckOutcome::Failed` is not only a failed check — `install_report`
  // (`update/mod.rs`) publishes it for a *refused install* too, after a check
  // that succeeded. Before this fix, `outcomeSentence` named the check
  // ("La dernière vérification a échoué") regardless of which operation had
  // actually failed, which read as a lie directly above `UpdateCard`'s red
  // line naming the install refusal. This must fail against the reverted
  // fix: reverting `update_last_attempt_failed` back to naming the check
  // would still pass the shared-outcome loop above (it never varies the
  // scenario), which is exactly how the regression slipped through — this
  // test is what closes that gap by inspecting the sentence itself.
  it('gives a failed outcome a sentence that does not claim the check itself failed', () => {
    const w = mount(UpdateSummary, {
      props: {
        update: payload({
          outcome: { kind: 'failed', detail: 'radio ne peut pas être installé depuis ici : …' },
        }),
      },
    })
    const text = w.find('[data-update-summary]').text()
    expect(text).toBe(CATALOG.update_last_attempt_failed)
    expect(text).not.toContain('vérification')
  })

  it('folds the detail away by default and lists both numbers in order when opened', async () => {
    // Installed then offered, as the current line already does, "so that a
    // reader notices an inversion". Folded because the page has several
    // cards and eleven lines would push the rest off a phone screen.
    //
    // The two rows are given distinct numbers (m9): identical rows could not
    // tell a preserved order from a reversed one, so the "lists ... in
    // order" half of this test's name was unproven — any permutation of two
    // identical rows passes an assertion that only inspects `rows[0]`.
    const components: ComponentOffer[] = [
      component('plugin-0', '0.1.0', '0.2.0'),
      component('plugin-1', '0.3.0', '0.4.0'),
    ]
    const w = mount(UpdateSummary, {
      props: { update: payload({ components }) },
    })
    expect(w.findAll('[data-update-detail-row]')).toHaveLength(0)
    await w.find('[data-update-detail-toggle]').trigger('click')
    const rows = w.findAll('[data-update-detail-row]')
    expect(rows).toHaveLength(2)
    expect(rows[0]!.text()).toContain('plugin-0')
    expect(rows[0]!.text()).toContain('0.1.0 → 0.2.0')
    expect(rows[1]!.text()).toContain('plugin-1')
    expect(rows[1]!.text()).toContain('0.3.0 → 0.4.0')
  })
})
