import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { UpdatePayload } from '../types'
import UpdateCard from './UpdateCard.vue'

// The catalog keys this card reads are already shipped in `en.toml` (Ruling
// 43), so this fixture is a literal copy of the real English strings rather
// than an invented one — a drift between the two would not be caught by a
// fixture that made something up instead.
const CATALOG = {
  update_title: 'Updates',
  update_no_release: 'No release published yet',
  update_only_prereleases:
    'Only prereleases are published; tick “Offer prereleases” below to be offered them',
  update_never_checked: 'Never checked',
  update_aligned: 'Up to date',
  update_unknown: 'Unknown',
  update_rolled_back: 'The update did not start and the previous version was put back',
  update_rollback_failed:
    'The update did not start, and putting the previous version back did not fully succeed — see the log',
  update_archive_notes:
    'The version now installed also carried {count} files that were not installed — see its release notes',
  update_partial_failure_note:
    'If several components were involved, only the first failure is shown here — see the log for the rest',
  update_release_notes: 'Release notes',
  update_check: 'Check for updates',
  update_install: 'Install',
}

// The card is handed its payload as a prop and never fetches anything itself
// (see UpdateCard.vue), but it still reads the shared catalog through
// `useCatalog` for every label it shows — so that singleton must be primed
// before each mount, the same way `CoverCacheDetails.test.ts` primes it for a
// component one level up.
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

// A payload builder, so each test states only what it is about.
function payload(over: Partial<UpdatePayload> = {}): UpdatePayload {
  return {
    outcome: { kind: 'ok' },
    release_version: '0.3.0',
    release_url: 'https://github.com/skerdudou/ritornello/releases/tag/v0.3.0',
    last_check_unix_s: 1_760_000_000,
    components: [
      {
        name: 'core',
        kind: 'core',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
    ],
    busy: null,
    last_rollback: null,
    ...over,
  }
}

function mountCard(update: UpdatePayload) {
  return mount(UpdateCard, { props: { update } })
}

describe('UpdateCard', () => {
  it('says a version is available, with both numbers', () => {
    const w = mountCard(payload())
    // The whole rendered sentence, with toBe: asserting `toContain('0.3.0')`
    // would pass just as well if the two numbers were swapped.
    expect(w.get('[data-update-summary]').text()).toBe('0.2.0 → 0.3.0')
  })

  it('says nothing has been published rather than showing a fault', () => {
    const w = mountCard(payload({ outcome: { kind: 'no_release' }, release_version: null }))
    expect(w.get('[data-update-summary]').text()).toBe('No release published yet')
    expect(w.find('[data-update-error]').exists()).toBe(false)
  })

  // The payload is the shape the core really sends for this outcome: it
  // rebuilds every row against an empty offer, so the core row is `unknown`
  // with no offered version. That is what makes this test discriminating —
  // read as anything but its own sentence, this payload falls through to
  // "Up to date", which is the defect: a beta was published, the switch was
  // off, and the card claimed the device was current.
  it('names the switch when only prereleases are published', () => {
    const w = mountCard(
      payload({
        outcome: { kind: 'only_prereleases' },
        release_version: null,
        components: [
          {
            name: 'core',
            kind: 'core',
            declared: true,
            binary_present: true,
            installed: '0.2.0',
            offered: null,
            availability: 'unknown',
          },
        ],
      }),
    )
    expect(w.get('[data-update-summary]').text()).toBe(
      'Only prereleases are published; tick “Offer prereleases” below to be offered them',
    )
    // A statement, not a fault: nothing failed here.
    expect(w.find('[data-update-error]').exists()).toBe(false)
  })

  it('shows a failed check as an error, with the reason', () => {
    const w = mountCard(payload({ outcome: { kind: 'failed', detail: 'HTTP 403' } }))
    expect(w.get('[data-update-error]').text()).toBe('HTTP 403')
  })

  it('disables both buttons and shows what is happening while busy', () => {
    const w = mountCard(payload({ busy: 'Installing radio…' }))
    expect(w.get('[data-update-busy]').text()).toBe('Installing radio…')
    expect(w.get('[data-update-check]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-update-install]').attributes('disabled')).toBeDefined()
  })

  it('offers no install when everything is aligned', () => {
    const w = mountCard(
      payload({
        components: [
          {
            name: 'core',
            kind: 'core',
            declared: true,
            binary_present: true,
            installed: '0.3.0',
            offered: '0.3.0',
            availability: 'aligned',
          },
        ],
      }),
    )
    expect(w.get('[data-update-install]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-update-summary]').text()).toBe('Up to date')
  })

  /// Without this the only trace of a 3 a.m. rollback is a version number
  /// that did not move.
  it('reports a rollback that happened while nobody was watching', () => {
    const w = mountCard(
      payload({
        last_rollback: {
          at_unix_s: 1_760_000_500,
          restored: ['core'],
          failed: [],
          core_restored: true,
        },
      }),
    )
    expect(w.get('[data-update-rollback]').text()).toBe(
      'The update did not start and the previous version was put back',
    )
  })

  /// **A rollback that put nothing back must not read like one that
  /// succeeded.** The unit writes an empty `restored` and a populated
  /// `failed` when the backup manifest is corrupt, and then deliberately does
  /// not restart the service — so the device is down, and the sentence the
  /// operator eventually reads used to tell them the previous version was in
  /// place.
  ///
  /// Two rows, because a single one could not tell the two clauses apart: the
  /// first restored nothing at all, the second restored one thing and failed
  /// on another. Both are "not a clean rollback", and each on its own must
  /// take the other sentence.
  it.each([
    ['nothing was restored', [] as string[], ['backup manifest at /var/…: expected value']],
    ['something could not be restored', ['radio'], ['core: Permission denied']],
  ])('does not claim the previous version is back when %s', (_why, restored, failed) => {
    const w = mountCard(
      payload({
        last_rollback: { at_unix_s: 1_760_000_500, restored, failed, core_restored: false },
      }),
    )
    expect(w.get('[data-update-rollback]').text()).toBe(
      'The update did not start, and putting the previous version back did not fully succeed — see the log',
    )
  })

  it('emits check when the button is pressed', async () => {
    const w = mountCard(payload())
    await w.get('[data-update-check]').trigger('click')
    expect(w.emitted('check')).toHaveLength(1)
  })

  it('emits install when the button is pressed and something is actionable', async () => {
    const w = mountCard(payload())
    await w.get('[data-update-install]').trigger('click')
    expect(w.emitted('install')).toHaveLength(1)
  })

  // Ruling 50-1: the core's own archive always exempts something (the
  // privileged installer, the systemd units, the polkit rules), and the page
  // must say so — a release that fixes the unit that runs self-update cannot
  // be delivered by the mechanism it fixes, and nobody was told before this.
  it('says how many files the last core install did not touch', () => {
    const w = mountCard(
      payload({
        components: [
          {
            name: 'core',
            kind: 'core',
            declared: true,
            binary_present: true,
            installed: '0.3.0',
            offered: '0.3.0',
            availability: 'aligned',
            not_installed_files: [
              'etc/systemd/system/ritornello-update.service',
              'usr/local/lib/ritornello/ritornello-update',
            ],
          },
        ],
      }),
    )
    expect(w.get('[data-update-core-notes]').text()).toBe(
      'The version now installed also carried 2 files that were not installed — see its release notes',
    )
  })

  it('says nothing about archive notes before any core install has happened', () => {
    const w = mountCard(payload())
    expect(w.find('[data-update-core-notes]').exists()).toBe(false)
  })

  // Ruling 50-2: this payload has one message field for a gesture that can
  // touch five components. The card states that limit plainly rather than
  // pretending the shown cause is the only one, in a line the exact-text
  // assertion on `[data-update-error]` above cannot see and so cannot break.
  it('says plainly that a failure names only the first cause', () => {
    const w = mountCard(payload({ outcome: { kind: 'failed', detail: 'HTTP 403' } }))
    expect(w.get('[data-update-error-note]').text()).toBe(
      'If several components were involved, only the first failure is shown here — see the log for the rest',
    )
  })

  it('has no partial-failure note when there is no failure to caveat', () => {
    const w = mountCard(payload())
    expect(w.find('[data-update-error-note]').exists()).toBe(false)
  })
})
