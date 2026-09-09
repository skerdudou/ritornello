import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { ComponentOffer } from '../types'
import UpdateDialog from './UpdateDialog.vue'

const CATALOG = {
  update_dialog_title: 'Confirm the update',
  update_dialog_description: 'Choose what to install. What is already up to date is left unchecked.',
  update_row_third_party: 'Third-party plugin from {repo}. Never selected automatically.',
  update_row_core_not_selected:
    '{component} will move while the core stays behind — this may make them incompatible.',
  update_confirm: 'Install selected',
}

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
  document.body.innerHTML = ''
})

function core(availability: ComponentOffer['availability'] = 'update_available'): ComponentOffer {
  return {
    name: 'core',
    kind: 'core',
    declared: true,
    binary_present: true,
    installed: '0.2.0',
    offered: availability === 'update_available' ? '0.3.0' : '0.2.0',
    availability,
  }
}

// The dialog's content is teleported (`DialogPortal`), so it lands in
// `document.body` regardless of where the component is mounted — same
// convention as `CoverCacheDetails.test.ts`. Mounted already `open`: unlike
// the panel above it, this dialog has no trigger of its own to click through.
function mountDialog(components: ComponentOffer[]) {
  return mount(UpdateDialog, { props: { open: true, components }, attachTo: document.body })
}

function row(name: string) {
  return document.body.querySelector(`[data-update-row][data-name="${name}"]`)
}

function isChecked(name: string): string | null {
  return row(name)?.querySelector('[data-update-row-check]')?.getAttribute('aria-checked') ?? null
}

describe('UpdateDialog', () => {
  it('pre-checks what is out of step and nothing else', async () => {
    mountDialog([
      core(),
      {
        name: 'radio',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
      // not_installed → unchecked, because choosing what is installed is the
      // operator's decision.
      {
        name: 'mpd',
        kind: 'plugin',
        declared: false,
        binary_present: false,
        installed: null,
        offered: '0.3.0',
        availability: 'not_installed',
      },
    ])
    await flushPromises()
    expect(isChecked('core')).toBe('true')
    expect(isChecked('radio')).toBe('true')
    expect(isChecked('mpd')).toBe('false')
  })

  it('leaves a third-party plugin unchecked and shows where it comes from', async () => {
    mountDialog([
      core('aligned'),
      {
        name: 'someones-plugin',
        kind: 'third_party',
        declared: true,
        binary_present: true,
        installed: '1.4.0',
        // `update_available`, not `unknown`: a real third-party row never
        // carries this (its own repository decides), but the guard under
        // test here is `kind !== 'third_party'` specifically, and the
        // sibling `availability === 'update_available'` filter must not be
        // what is doing the excluding — an `unknown` row is excluded by
        // *that* filter regardless of kind, which would let the kind guard
        // be deleted with every test in this file still green.
        offered: '2.0.0',
        availability: 'update_available',
        third_party_repo: 'someone/their-plugin',
      },
    ])
    await flushPromises()
    // Never pre-checked, never taken by the automatic policy either — this
    // is the same exclusion, read from the dialog's own default.
    expect(isChecked('someones-plugin')).toBe('false')
    expect(row('someones-plugin')?.querySelector('[data-update-row-warning]')?.textContent).toBe(
      'Third-party plugin from someone/their-plugin. Never selected automatically.',
    )
  })

  it('refuses to submit with nothing checked', async () => {
    mountDialog([core('aligned')])
    await flushPromises()
    const confirm = document.body.querySelector<HTMLButtonElement>('[data-update-confirm]')
    // Nothing is out of step, so the default leaves every row unchecked: an
    // empty install is not a gesture.
    expect(confirm?.disabled).toBe(true)
  })

  it('warns when a plugin is checked and the core is not, at differing versions', async () => {
    mountDialog([
      core(), // update_available: pre-checked by default
      {
        name: 'radio',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
    ])
    await flushPromises()
    // No warning yet: the core is out of step too, but it is (by default)
    // checked right alongside radio — nothing is being left behind.
    expect(row('radio')?.querySelector('[data-update-row-warning]')).toBeNull()

    // The operator unchecks the core by hand, radio stays checked: now the
    // core's own offered version (0.3.0) will not be installed while radio's
    // is — the refusal screen from the previous chantier is the backstop,
    // this is the warning that comes before it.
    await row('core')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('false')
    expect(isChecked('radio')).toBe('true')
    expect(row('radio')?.querySelector('[data-update-row-warning]')?.textContent).toBe(
      'radio will move while the core stays behind — this may make them incompatible.',
    )
  })

  it('does not warn about an unchecked plugin, even while the core is left behind', async () => {
    // The operand the "warns when a plugin is checked" test title promises
    // but, on its own, does not pin: `mpd` here is never checked (it is
    // `not_installed`, excluded by the same default as in the first test),
    // yet the core ends up left behind exactly as in the warning test above.
    // Without the `checked.value.has(c.name)` guard, every plugin row would
    // warn whenever the core is left behind, checked or not.
    mountDialog([
      core(),
      {
        name: 'mpd',
        kind: 'plugin',
        declared: false,
        binary_present: false,
        installed: null,
        offered: '0.3.0',
        availability: 'not_installed',
      },
    ])
    await flushPromises()
    expect(isChecked('mpd')).toBe('false')

    await row('core')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('false')
    expect(isChecked('mpd')).toBe('false')
    expect(row('mpd')?.querySelector('[data-update-row-warning]')).toBeNull()
  })

  it('does not warn about a checked plugin when the core has nothing to update', async () => {
    // The other operand of the same predicate: the core is unchecked here
    // too (nothing pre-checks an aligned core), but there is no update it
    // could be "left behind" from, so a manually-checked plugin gets no
    // warning either.
    mountDialog([
      core('aligned'),
      {
        name: 'radio',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
    ])
    await flushPromises()
    expect(isChecked('core')).toBe('false')
    expect(isChecked('radio')).toBe('true')
    expect(row('radio')?.querySelector('[data-update-row-warning]')).toBeNull()
  })

  it('emits confirm with exactly the checked names', async () => {
    const w = mountDialog([
      core(),
      {
        name: 'radio',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
    ])
    await flushPromises()
    document.body.querySelector<HTMLButtonElement>('[data-update-confirm]')!.click()
    await flushPromises()
    const emitted = w.emitted('confirm')
    expect(emitted).toHaveLength(1)
    const names = (emitted?.[0]?.[0] as string[] | undefined) ?? []
    expect([...names].sort()).toEqual(['core', 'radio'])
  })

  it('resets its selection every time it reopens, rather than keeping a stale hand-check', async () => {
    const w = mountDialog([core('aligned')])
    await flushPromises()
    // Hand-check a row that the default policy would leave unchecked (core
    // is aligned here, nothing to do), then close and reopen with the same,
    // still-aligned component. If the reset did not run, the hand-check
    // would still read `true` — the one outcome this test cannot get by
    // accident, since the fresh default for an aligned row is `false`.
    await row('core')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('true')

    await w.setProps({ open: false })
    await w.setProps({ open: true, components: [core('aligned')] })
    await flushPromises()
    expect(isChecked('core')).toBe('false')
  })

  // Ruling 88: the guard is `offered === null`, never `kind` — a third-party
  // row whose own repository could not be consulted, and an official plugin
  // this release does not carry, must both refuse the click, not merely skip
  // the pre-check.
  it('a third-party row with no offer at all cannot be hand-checked', async () => {
    mountDialog([
      core('aligned'),
      {
        name: 'someones-plugin',
        kind: 'third_party',
        declared: true,
        binary_present: true,
        installed: '1.4.0',
        offered: null,
        availability: 'unknown',
        third_party_repo: 'someone/their-plugin',
      },
    ])
    await flushPromises()
    expect(isChecked('someones-plugin')).toBe('false')
    await row('someones-plugin')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('someones-plugin')).toBe('false')
  })

  it('an official plugin this release does not carry cannot be hand-checked either', async () => {
    // Same guard, kind-agnostic: disabling only `third_party` rows would have
    // left this one clickable, offering an install that could only fail.
    mountDialog([
      core('aligned'),
      {
        name: 'legacy',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.1.0',
        offered: null,
        availability: 'unknown',
      },
    ])
    await flushPromises()
    expect(isChecked('legacy')).toBe('false')
    await row('legacy')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('legacy')).toBe('false')
  })

  // I5 (fix round 1): the two tests above only pin the `offered === null`
  // side of the guard. A mutation replacing it with
  // `kind === 'third_party' || offered === null` — exactly the "disable
  // every third-party row" instinct ruling 88 overruled — passed every test
  // in this file, because the *other* third-party test above never clicks
  // the switch. This one does, and it is the operand that mutation deletes.
  it('a third-party row with a real offer stays hand-checkable', async () => {
    mountDialog([
      core('aligned'),
      {
        name: 'someones-plugin',
        kind: 'third_party',
        declared: true,
        binary_present: true,
        installed: '1.4.0',
        offered: '2.0.0',
        availability: 'update_available',
        third_party_repo: 'someone/their-plugin',
      },
    ])
    await flushPromises()
    // Never pre-checked (task 17's own default), but reachable by hand.
    expect(isChecked('someones-plugin')).toBe('false')
    await row('someones-plugin')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('someones-plugin')).toBe('true')
  })
})
