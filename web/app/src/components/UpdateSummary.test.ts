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
const CATALOG = {
  update_out_of_step: '{count} composants à mettre à jour vers {version}',
  update_out_of_step_one: '{name} : {installed} → {offered}',
  update_aligned: 'À jour',
  update_no_release: "Aucune version publiée pour l'instant",
  update_only_prereleases:
    'Seules des préversions sont publiées ; cochez « Proposer les préversions » ci-dessous pour qu\'elles vous soient proposées',
  update_never_checked: 'Jamais vérifié',
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
  } = {},
): UpdatePayload {
  const components: ComponentOffer[] =
    over.only === 'core'
      ? [{ ...component('cœur', over.from!, over.to!), kind: 'core' }]
      : Array.from({ length: over.outOfStep ?? 2 }, (_, i) =>
          component(`plugin-${i}`, '0.2.0', '0.2.0-beta.1'),
        )
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
    expect(w.find('[data-update-summary]').text()).toBe('cœur : 0.2.0 → 0.2.0-beta.1')
  })

  it('says up to date when nothing is out of step', () => {
    const w = mount(UpdateSummary, { props: { update: payload({ outOfStep: 0 }) } })
    expect(w.find('[data-update-summary]').text()).toBe('À jour')
  })

  it('keeps the sentence of an outcome that counts nothing', () => {
    // Never checked, nothing published, only prereleases, last check failed:
    // each rebuilt every row against an empty offer, so a count would be a
    // claim about a device that has not looked.
    for (const kind of ['never_checked', 'no_release', 'only_prereleases'] as const) {
      const w = mount(UpdateSummary, { props: { update: payload({ outcome: { kind } }) } })
      expect(w.find('[data-update-summary]').text()).not.toContain('composants')
    }
  })

  it('folds the detail away by default and lists both numbers in order when opened', async () => {
    // Installed then offered, as the current line already does, "so that a
    // reader notices an inversion". Folded because the page has several
    // cards and eleven lines would push the rest off a phone screen.
    const w = mount(UpdateSummary, { props: { update: payload({ outOfStep: 2 }) } })
    expect(w.findAll('[data-update-detail-row]')).toHaveLength(0)
    await w.find('[data-update-detail-toggle]').trigger('click')
    const rows = w.findAll('[data-update-detail-row]')
    expect(rows).toHaveLength(2)
    expect(rows[0]!.text()).toContain('0.2.0 → 0.2.0-beta.1')
  })
})
