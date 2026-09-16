import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { SettingsPayload, UpdatePayload } from '../types'
import UpdateCard from './UpdateCard.vue'

// The catalog keys this card reads are already shipped in `en.toml` (Ruling
// 43), so this fixture is a literal copy of the real English strings rather
// than an invented one — a drift between the two would not be caught by a
// fixture that made something up instead.
const CATALOG = {
  update_title: 'Updates',
  update_no_release: 'No release published yet',
  update_only_prereleases:
    'Only prereleases are published; tick “Offer beta versions” below to be offered them',
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
  // Read by `UpdateSummary.vue`, mounted inside this card since Task 5: the
  // summary line moved to its own component, but this card's fixture still
  // has to prime it, the same way it primes every other key the card reads
  // through a child.
  update_out_of_step: '{count} components to update to {version}',
  update_out_of_step_one: '{name}: {installed} → {offered}',
  update_detail: 'Detail',
  update_release_notes: 'Release notes',
  update_check: 'Check for updates',
  update_install: 'Install',
  // Task 6: the automatic-checks policy, folded into this card below a
  // separator, and the beta switch that sits above it.
  update_policy_label: 'Automatic checks',
  update_policy_off: 'Off',
  update_policy_check: 'Check only',
  update_policy_check_and_install: 'Check and install',
  update_hour_label: 'Hour',
  update_cadence_label: 'Cadence',
  update_cadence_daily: 'Daily',
  update_cadence_weekly: 'Weekly',
  update_cadence_day_label: 'Day',
  update_prereleases_label: 'Offer beta versions',
  update_prereleases_help:
    'Including when you check by hand; a finished release replaces them as soon as one is published.',
  weekday_sunday: 'Sunday',
  weekday_monday: 'Monday',
  weekday_tuesday: 'Tuesday',
  weekday_wednesday: 'Wednesday',
  weekday_thursday: 'Thursday',
  weekday_friday: 'Friday',
  weekday_saturday: 'Saturday',
  save: 'Save',
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

// A settings builder, mirroring the shape `ConfigView.vue` keeps as its own
// default (the fields this card actually reads and writes; the rest of
// `SettingsPayload` is filled with values this card never touches).
function settings(over: Partial<SettingsPayload> = {}): SettingsPayload {
  return {
    volume_repeat_initial_ms: 800,
    volume_repeat_interval_ms: 200,
    startup_power: 'on',
    date_format: 'day_month_year',
    clock_24h: true,
    overlay_ms: 5000,
    tens_window_ms: 5000,
    seek_step_s: 10,
    cover_cache_budget_mio: 50,
    cover_download_max_mio: 2,
    cover_source_max_mio: 20,
    cover_rendition: true,
    cover_max_edge_px: 640,
    cover_jpeg_quality: 85,
    cover_passthrough_max_ko: 150,
    cover_max_pixels_mpx: 16,
    update_policy: 'off',
    update_hour: 3,
    update_cadence: { kind: 'daily' },
    update_prereleases: false,
    ...over,
  }
}

function mountCard(update: UpdatePayload) {
  return mount(UpdateCard, { props: { update, settings: settings() } })
}

describe('UpdateCard', () => {
  // Task 5: the line used to name the core alone and say nothing when other
  // components were out of step — the very defect an owner reported after a
  // beta offered ten plugins the card stayed silent about. The line now
  // counts every component out of step and, for exactly one, names it rather
  // than showing a bare pair (a payload with only the core out of step is
  // this single-component case, so it still reads as a name and two
  // numbers, just no longer bare).
  it('says a version is available, naming the one component and both numbers', () => {
    const w = mountCard(payload())
    // The whole rendered sentence, with toBe: asserting `toContain('0.3.0')`
    // would pass just as well if the two numbers were swapped.
    expect(w.get('[data-update-summary]').text()).toBe('core: 0.2.0 → 0.3.0')
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
      'Only prereleases are published; tick “Offer beta versions” below to be offered them',
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

  it('holds one save path, and the two action buttons are not it', async () => {
    // The separator carries the card's meaning: above it, two buttons that
    // act at once; below it, settings that wait. That is why merging does
    // not break the written decision refusing "two save paths behind one
    // title" — there is only one.
    //
    // m9: the original body only ever clicked Check, so half the claim
    // ("the two action buttons") was unproven — a regression wiring Install
    // to `save` instead of `install` would have passed this test unnoticed.
    // `payload()`'s default component is `update_available`, so Install is
    // not disabled here.
    const w = mount(UpdateCard, { props: { update: payload(), settings: settings() } })
    expect(w.findAll('[data-update-save]')).toHaveLength(1)
    await w.find('[data-update-check]').trigger('click')
    expect(w.emitted('save')).toBeUndefined()
    expect(w.emitted('check')).toHaveLength(1)
    await w.find('[data-update-install]').trigger('click')
    expect(w.emitted('save')).toBeUndefined()
    expect(w.emitted('install')).toHaveLength(1)
    await w.find('[data-update-save]').trigger('click')
    expect(w.emitted('save')).toHaveLength(1)
  })

  it('puts the beta switch and the automatic policy in one card, two lines apart', () => {
    // The argument for "Offer" rather than "Install" is now visible to the
    // eye: installing depends on the policy right below it.
    //
    // m9: existence alone (`.exists()`) does not prove either "two lines
    // apart" or even an order between the two — a policy control rendered
    // above the switch, or anywhere else on the page, would have passed.
    // `compareDocumentPosition` proves the switch's line comes first.
    const w = mount(UpdateCard, { props: { update: payload(), settings: settings() } })
    const prereleases = w.get('[data-update-prereleases]').element
    const policy = w.get('[data-update-policy]').element
    expect(prereleases.compareDocumentPosition(policy) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
    // "Two lines apart": the switch's own line and the policy's own line are
    // adjacent siblings under the same section, with nothing else rendered
    // by this card between them.
    const prereleasesLine = prereleases.closest('label')!
    expect(prereleasesLine.nextElementSibling?.contains(policy)).toBe(true)
  })
})
