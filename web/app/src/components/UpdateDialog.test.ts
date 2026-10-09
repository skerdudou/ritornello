import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { ComponentOffer, Fit } from '../types'
import UpdateDialog from './UpdateDialog.vue'

const CATALOG = {
  update_dialog_title: 'Confirm the update',
  update_dialog_description: 'Only what needs updating is listed. Choose what to install.',
  update_row_third_party: 'Third-party plugin from {repo}. Never selected automatically.',
  update_row_core_not_selected:
    '{component} will move while the core stays behind — this may make them incompatible.',
  update_row_plugins_not_selected:
    'The core will move while these plugins stay behind: {components}. This core may refuse them until they are updated too.',
  update_confirm: 'Install selected',
  plugin_privileged_note:
    'Privileged component: install or uninstall it with ritornello-install. An update that leaves its root-run companion unchanged can be made from here.',
  update_row_needs_companion:
    'This update also changes the mount helper ({companion}): update with ritornello-install.',
  update_row_third_party_pack: 'From a third-party repository. Never selected automatically.',
  update_row_third_party_refused:
    'Its last archive was refused: a third-party plugin\'s archive may carry nothing but its own binary.',
  update_row_pack_refused: 'Its last archive was refused by the language pack checks; a new version will be tried.',
  update_row_pack_official: '{language} language pack (Ritornello)',
  update_row_pack_third_party: '{language} language pack from {repo}',
  // Literal copies of `en.toml`, like every key above.
  update_row_contracts_unpublished:
    '{component} cannot be installed from this device: its release does not publish which protocol versions it speaks. Installing it by hand, outside the device, remains possible.',
  update_row_fit_gap: '{contract} {found} vs {expected}',
  update_row_fit_limited: 'Limited with this core: {contracts}. Some of its features will be inactive.',
  update_row_fit_refused: 'Will be refused by this core: {reason}',
  update_row_fit_refused_unless_core:
    'Will be refused by the running core unless the core is updated too: {reason}',
  update_row_refused_until_updated: 'Left as it is, it will be refused by the new core until it is updated.',
  update_major_banner:
    'Major update: the core changes how it talks to its plugins. Plugins left unticked or not updated will be refused until they are updated.',
  update_major_refused: 'Refused by the new core until updated: {components}',
  plugin_kind_source: 'source',
  plugin_incompatible_major: 'Incompatible {contract} contract: built for {found}, this core speaks {expected}',
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
  it('lists what is out of step and nothing else', async () => {
    // A component the device does not have belongs in the installables
    // dialog, not here: mixing "what moved" with "what you could add" is
    // what made this screen unreadable.
    mountDialog([
      core(),
      {
        name: 'console',
        kind: 'plugin',
        declared: false,
        binary_present: false,
        installed: null,
        offered: '0.3.0',
        availability: 'not_installed',
      },
    ])
    await flushPromises()
    const names = [...document.body.querySelectorAll('[data-update-row-name]')].map((n) =>
      n.textContent?.trim(),
    )
    expect(names).toEqual(['core'])
  })

  it('keeps a declared component whose binary is missing', async () => {
    // Installing IS the repair for that row, and it is a component the
    // device declares — so it is not "something you could add".
    mountDialog([
      {
        name: 'cd',
        kind: 'plugin',
        declared: true,
        binary_present: false,
        installed: null,
        offered: '0.3.0',
        availability: 'binary_missing',
      },
    ])
    await flushPromises()
    expect(document.body.querySelectorAll('[data-update-row-name]')).toHaveLength(1)
  })

  it('keeps a third-party row that has an offer', async () => {
    // Its repository answered: it can genuinely be updated from there, and
    // the row carries the warning that says where the bytes come from.
    mountDialog([
      {
        name: 'x',
        kind: 'third_party',
        declared: true,
        binary_present: true,
        installed: '1.0.0',
        offered: '2.0.0',
        availability: 'update_available',
        third_party_repo: 'owner/repo',
      },
    ])
    await flushPromises()
    expect(document.body.querySelectorAll('[data-update-row-name]')).toHaveLength(1)
  })

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
      // not_installed → excluded from the dialog entirely (task 7): choosing
      // what to install is a different question, asked by
      // `InstallablesDialog.vue`, not this one.
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
    expect(row('mpd')).toBeNull()
  })

  /// **The third of the dialog's three exclusions, and the one nothing
  /// watched.** `docs/interface.md` says a component already known to need a
  /// manual step is not pre-ticked, because ticking it again would only repeat
  /// the same refusal — and deleting `.filter((c) => c.installable !== false)`
  /// left all 89 tests in this file and `ConfigView.test.ts` green, since no
  /// fixture anywhere carried `installable: false`.
  ///
  /// The row is one of **ours**, `update_available`, declared and installed,
  /// so every sibling filter passes it: `installable` is the only thing that
  /// can be excluding it. `radio` beside it is the positive control — without
  /// it, a `defaultChecked` that returned an empty set would pass.
  it('leaves a component known to need a manual step unchecked', async () => {
    mountDialog([
      {
        name: 'files',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
        installable: false,
      },
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
    expect(isChecked('files')).toBe('false')
    expect(isChecked('radio')).toBe('true')
  })

  // Fix round 1, M1: `defaultChecked` already left `files` unticked (the
  // test above), but the switch itself stayed enabled, so an operator could
  // still tick it by hand and confirm — reaching `installable_from_ui`'s
  // refusal after a download, the exact "button that then fails" this whole
  // change exists to remove. Both halves: `files`'s switch refuses the
  // click and shows the sentence, `radio`'s switch on the same page still
  // takes the click and shows nothing.
  it('refuses a hand click on a privileged plugin and explains why, leaving an ordinary one untouched', async () => {
    mountDialog([
      {
        name: 'files',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
        installable: false,
      },
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
    expect(isChecked('files')).toBe('false')
    await row('files')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('files')).toBe('false')
    expect(row('files')?.querySelector('[data-update-row-warning]')?.textContent).toContain(
      'ritornello-install',
    )

    expect(row('radio')?.querySelector('[data-update-row-warning]')).toBeNull()
    // `radio` is `update_available` and not privileged, so it is pre-checked
    // by default; a hand click still takes effect (untoggling it), unlike
    // `files`'s switch above, which never moved at all.
    expect(isChecked('radio')).toBe('true')
    await row('radio')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('radio')).toBe('false')
  })

  // A row the core refused because its companion moved says so, naming the
  // companion — what to do about this very update — and a row refused for
  // another reason keeps the privileged sentence. Both halves, compared
  // exactly: both sentences name ritornello-install, so a `toContain` on
  // that could not tell them apart.
  it('says the companion moved when the core says so, and the privileged rule otherwise', async () => {
    const files: ComponentOffer = {
      name: 'files',
      kind: 'plugin',
      declared: true,
      binary_present: true,
      installed: '0.2.0',
      offered: '0.3.0',
      availability: 'update_available',
      installable: false,
    }
    const first = mountDialog([{ ...files, needs_companion: 'files-mount' }])
    await flushPromises()
    expect(row('files')?.querySelector('[data-update-row-warning]')?.textContent?.trim()).toBe(
      'This update also changes the mount helper (files-mount): update with ritornello-install.',
    )
    expect(isChecked('files')).toBe('false')
    first.unmount()

    mountDialog([files])
    await flushPromises()
    expect(row('files')?.querySelector('[data-update-row-warning]')?.textContent?.trim()).toBe(
      CATALOG.plugin_privileged_note,
    )
  })

  // The other half of the test above: a privileged plugin the device
  // declares may be updated from here while its companion does not move,
  // which the core settles at the gesture. Until an attempt
  // is refused, its row carries no `installable` at all, and the switch must
  // follow that flag rather than the plugin's name: enabled, pre-checked like
  // any update, and without the privileged sentence.
  it('lets an installed privileged plugin be updated while nothing says otherwise', async () => {
    mountDialog([
      {
        name: 'files',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
    ])
    await flushPromises()
    const check = row('files')!.querySelector<HTMLElement>('[data-update-row-check]')!
    expect(check.hasAttribute('disabled')).toBe(false)
    expect(isChecked('files')).toBe('true')
    expect(row('files')?.querySelector('[data-update-row-warning]')).toBeNull()
    await check.click()
    await flushPromises()
    expect(isChecked('files')).toBe('false')
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
    // but, on its own, does not pin: `files` here is unchecked by hand, not
    // by `installable: false` (fix round 1, M1 gave that flag its own
    // warning, which would otherwise sit in this row and defeat the "no
    // warning at all" assertion below for an unrelated reason) — yet the
    // core ends up left behind exactly as in the warning test above.
    // Without the `checked.value.has(c.name)` guard, every plugin row would
    // warn whenever the core is left behind, checked or not.
    mountDialog([
      core(),
      {
        name: 'files',
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
      },
    ])
    await flushPromises()
    // Pre-checked by default, like `core` and `radio` above; unchecked here
    // by hand so the row is genuinely unchecked without needing
    // `installable: false`.
    expect(isChecked('files')).toBe('true')
    await row('files')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('files')).toBe('false')

    await row('core')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('false')
    expect(isChecked('files')).toBe('false')
    expect(row('files')?.querySelector('[data-update-row-warning]')).toBeNull()
  })

  it('does not warn about a checked plugin when the core has nothing to update', async () => {
    // The other operand of the same predicate: the core has nothing to
    // update here (`aligned`), so it does not even appear as a row — but a
    // manually-checked plugin still gets no warning, because there is
    // nothing for it to be "left behind" from.
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
    expect(row('core')).toBeNull()
    expect(isChecked('radio')).toBe('true')
    expect(row('radio')?.querySelector('[data-update-row-warning]')).toBeNull()
  })

  function plugin(name: string, extra: Partial<ComponentOffer> = {}): ComponentOffer {
    return {
      name,
      kind: 'plugin',
      declared: true,
      binary_present: true,
      installed: '0.2.0',
      offered: '0.3.0',
      availability: 'update_available',
      ...extra,
    }
  }

  const coreWarning = () => row('core')?.querySelector('[data-update-row-warning]')?.textContent ?? null

  it('warns on the core row when it is checked and a plugin with an update is not', async () => {
    // The symmetrical warning: across a wire break, the new core refuses
    // every plugin left on the old side, and the page would turn red row by
    // row with no earlier word.
    mountDialog([core(), plugin('radio'), plugin('cd')])
    await flushPromises()
    // Everything ticked by default: nothing is left behind.
    expect(coreWarning()).toBeNull()

    await row('radio')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('true')
    expect(coreWarning()).toBe(
      'The core will move while these plugins stay behind: radio. This core may refuse them until they are updated too.',
    )

    await row('cd')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(coreWarning()).toBe(
      'The core will move while these plugins stay behind: radio, cd. This core may refuse them until they are updated too.',
    )
  })

  it('does not warn on the core row when the core itself is unchecked', async () => {
    mountDialog([core(), plugin('radio')])
    await flushPromises()
    await row('radio')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await row('core')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('false')
    expect(isChecked('radio')).toBe('false')
    expect(coreWarning()).toBeNull()
  })

  it('does not count a third-party plugin among those the core leaves behind', async () => {
    // Never ticked by default, and it carries its own warning: counting it
    // would put this warning on the core row of every such device.
    mountDialog([
      core(),
      plugin('x', { kind: 'third_party', third_party_repo: 'owner/repo', installed: '1.0.0', offered: '2.0.0' }),
      plugin('y', { third_party_repo: 'owner/other', installed: '1.0.0', offered: '2.0.0' }),
    ])
    await flushPromises()
    expect(isChecked('core')).toBe('true')
    expect(isChecked('x')).toBe('false')
    expect(isChecked('y')).toBe('false')
    expect(coreWarning()).toBeNull()
  })

  it('does not count a plugin that is not out of step', async () => {
    // A `binary_missing` row is listed but not `update_available`: it is a
    // repair, not a version the core could disagree with.
    mountDialog([core(), plugin('cd', { availability: 'binary_missing', installed: null })])
    await flushPromises()
    expect(isChecked('cd')).toBe('false')
    expect(coreWarning()).toBeNull()
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
    const w = mountDialog([core('binary_missing')])
    await flushPromises()
    // Hand-check a row that the default policy would leave unchecked
    // (`binary_missing` is not `update_available`, so nothing pre-checks
    // it, yet it stays in the dialog — installing is its own repair), then
    // close and reopen with the same, still-`binary_missing` component. If
    // the reset did not run, the hand-check would still read `true` — the
    // one outcome this test cannot get by accident, since the fresh default
    // for this row is `false`.
    await row('core')!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
    await flushPromises()
    expect(isChecked('core')).toBe('true')

    await w.setProps({ open: false })
    await w.setProps({ open: true, components: [core('binary_missing')] })
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
        // `binary_missing`, not `unknown`: the `relevant` filter (task 7)
        // hides `unknown` rows entirely, so reaching the disabled-switch
        // guard below needs a fixture that stays in `relevant` — a declared
        // component whose binary is gone can genuinely have no offer, when
        // this release's own archive does not carry a build for it either.
        binary_present: false,
        installed: null,
        offered: null,
        availability: 'binary_missing',
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
    // `binary_missing`, not `unknown`, for the same reason as the test
    // above: `unknown` never reaches this guard any more, `relevant` hides
    // it first.
    mountDialog([
      core('aligned'),
      {
        name: 'legacy',
        kind: 'plugin',
        declared: true,
        binary_present: false,
        installed: null,
        offered: null,
        availability: 'binary_missing',
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

  describe('language packs', () => {
    const XLANG = 'ritornello-xlang-fr-0123456789ab'
    const pack = (overrides: Partial<ComponentOffer> & { name: string }): ComponentOffer => ({
      kind: 'language_pack',
      declared: false,
      binary_present: false,
      installed: '1.0.0',
      offered: '1.1.0',
      availability: 'update_available',
      installable: true,
      ...overrides,
    })

    // H7: a third-party pack's row has the kind `language_pack`, and the
    // pre-tick used to read only `kind === 'third_party'` — so a stranger's
    // pack update was ticked like one of ours, against ruling P6. Ours, in
    // the same state, still is: a dialog that ticks no pack cannot pass.
    // **[MUTATION]** filter on the kind alone again: red on the stranger's.
    it('never pre-ticks a third-party pack, and still pre-ticks ours', async () => {
      mountDialog([pack({ name: 'ritornello-lang-fr' }), pack({ name: XLANG, third_party_repo: 'z/zed' })])
      await flushPromises()
      expect(isChecked('ritornello-lang-fr')).toBe('true')
      expect(isChecked(XLANG)).toBe('false')
    })

    // The kind half of the same guard: a third-party plugin row that names
    // no repository (none the core builds today) fails closed all the same.
    // **[MUTATION]** drop `c.kind !== 'third_party'`: red.
    it('never pre-ticks a third-party plugin row that names no repository', async () => {
      mountDialog([
        {
          name: 'orphan', kind: 'third_party', declared: true, binary_present: true,
          installed: '1.0.0', offered: '2.0.0', availability: 'update_available',
        },
      ])
      await flushPromises()
      expect(isChecked('orphan')).toBe('false')
    })

    // H7: a pack row is named by its language and source, never by its id
    // (a stranger's id is a digest). The switch's accessible name follows.
    // **[MUTATION]** render `offer.name` again: red.
    it('names a pack by its language and source, never by its id', async () => {
      mountDialog([pack({ name: 'ritornello-lang-fr' }), pack({ name: XLANG, third_party_repo: 'z/zed' })])
      await flushPromises()
      const label = (name: string) => row(name)?.querySelector('[data-update-row-name]')?.textContent?.trim()
      expect(label('ritornello-lang-fr')).toBe('Français language pack (Ritornello)')
      expect(label(XLANG)).toBe('Français language pack from z/zed')
      expect(document.body.querySelector('[data-update-dialog]')?.textContent).not.toContain(XLANG)
      expect(row(XLANG)?.querySelector('[data-update-row-check]')?.getAttribute('aria-label'))
        .toBe('Français language pack from z/zed')
      expect(row(XLANG)?.querySelector('[data-update-row-warning]')?.textContent?.trim())
        .toBe('From a third-party repository. Never selected automatically.')
    })

    // H5: a pack whose archive the reader refused is marked like a manual
    // step; its note says the pack was refused, never the privileged-plugin
    // sentence. **[MUTATION]** drop the pack branch of `refusedNote`: red.
    it('says a refused pack was refused, never that it is a privileged component', async () => {
      mountDialog([pack({ name: XLANG, third_party_repo: 'z/zed', installable: false })])
      await flushPromises()
      const warning = row(XLANG)?.querySelector('[data-update-row-warning]')?.textContent ?? ''
      expect(warning).toContain('language pack checks')
      expect(warning).not.toContain('ritornello-install')
    })

    // H5, the plugin half: a stranger's plugin whose archive carried more
    // than its binary. **[MUTATION]** drop the `third_party_repo` branch of
    // `refusedNote`: red.
    it('says a refused third-party plugin archive was refused for what it carried', async () => {
      mountDialog([
        {
          name: 'zed', kind: 'third_party', declared: true, binary_present: true, installed: '1.0.0',
          offered: '2.0.0', availability: 'update_available', third_party_repo: 'z/zed', installable: false,
        },
      ])
      await flushPromises()
      const warning = row('zed')?.querySelector('[data-update-row-warning]')?.textContent ?? ''
      expect(warning).toContain('nothing but its own binary')
      expect(warning).not.toContain('ritornello-install')
    })
  })
  describe('what each plugin becomes with the chosen core', () => {
    const v = (major: number, minor: number) => ({ major, minor })
    // The verdicts are the core's (`update::state::Fit`), handed over as is.
    const REFUSED: Fit = {
      fit: 'refused',
      refusal: { reason: 'major', gaps: [{ contract: 'source', plugin: v(1, 0), core: v(2, 0) }] },
    }
    const REFUSED_TEXT = 'Incompatible source contract: built for 1.0, this core speaks 2.0'
    const LIMITED: Fit = { fit: 'limited', gaps: [{ contract: 'source', plugin: v(2, 3), core: v(2, 1) }] }

    function offer(name: string, extra: Partial<ComponentOffer> = {}): ComponentOffer {
      return {
        name,
        kind: 'plugin',
        declared: true,
        binary_present: true,
        installed: '0.2.0',
        offered: '0.3.0',
        availability: 'update_available',
        ...extra,
      }
    }
    const breakingCore = (): ComponentOffer => ({ ...core(), breaking: true })
    const fit = (name: string) => row(name)?.querySelector('[data-update-row-fit]')?.textContent?.trim() ?? null
    const warning = (name: string) =>
      row(name)?.querySelector('[data-update-row-warning]')?.textContent?.trim() ?? null
    const banner = () => document.body.querySelector('[data-update-major]')
    const click = async (name: string) => {
      row(name)!.querySelector<HTMLElement>('[data-update-row-check]')!.click()
      await flushPromises()
    }

    it('says nothing more for a plugin compatible with the chosen core', async () => {
      mountDialog([core(), offer('radio', { with_core: { fit: 'compatible' }, with_running_core: { fit: 'compatible' } })])
      await flushPromises()
      expect(fit('radio')).toBeNull()
    })

    it('says which contracts a limited plugin will run without', async () => {
      mountDialog([core(), offer('radio', { with_core: LIMITED, with_running_core: { fit: 'compatible' } })])
      await flushPromises()
      expect(fit('radio')).toBe('Limited with this core: source 2.3 vs 2.1. Some of its features will be inactive.')
    })

    it('says the chosen core will refuse a plugin, judged against that core and not the running one', async () => {
      // The running core accepts it and the offered one does not: reading
      // `with_running_core` here would say nothing at all.
      mountDialog([core(), offer('radio', { with_core: REFUSED, with_running_core: { fit: 'compatible' } })])
      await flushPromises()
      expect(fit('radio')).toBe(`Will be refused by this core: ${REFUSED_TEXT}`)
    })

    it('says a plugin whose contracts are unpublished cannot be installed from here, and disables it', async () => {
      mountDialog([
        core(),
        offer('radio', { installable: false, not_installable_reason: 'contracts_unpublished' }),
      ])
      await flushPromises()
      expect(warning('radio')).toBe(
        'radio cannot be installed from this device: its release does not publish which protocol versions it speaks. Installing it by hand, outside the device, remains possible.',
      )
      expect(isChecked('radio')).toBe('false')
      expect(row('radio')?.querySelector('[data-update-row-check]')?.hasAttribute('disabled')).toBe(true)
      expect(fit('radio')).toBeNull()
    })

    it('warns that a dependent plugin ticked without the breaking core will be refused by the running core', async () => {
      // The worker installs it alone when the core is not part of the
      // gesture, and the running core refuses it.
      mountDialog([breakingCore(), offer('radio', { with_core: { fit: 'compatible' }, with_running_core: REFUSED })])
      await flushPromises()
      expect(fit('radio')).toBeNull()

      await click('core')
      expect(isChecked('radio')).toBe('true')
      expect(fit('radio')).toBe(`Will be refused by the running core unless the core is updated too: ${REFUSED_TEXT}`)
      // The precise word replaces the general "may", rather than repeating it.
      expect(warning('radio')).toBeNull()

      // Neither ticked: nothing moves, nothing to say.
      await click('radio')
      expect(fit('radio')).toBeNull()
    })

    it('shows the major banner only for a breaking core that is ticked, naming what it will refuse', async () => {
      mountDialog([
        breakingCore(),
        offer('radio', { with_core: { fit: 'compatible' }, with_running_core: REFUSED }),
        offer('cd', { with_core: { fit: 'compatible' }, with_running_core: REFUSED }),
        // Not updated by the release: judged from its live announcement.
        offer('zed', { kind: 'third_party', third_party_repo: 'z/zed', offered: null, availability: 'unknown', with_core: REFUSED }),
      ])
      await flushPromises()
      expect(banner()?.querySelector('p')?.textContent?.trim()).toBe(
        'Major update: the core changes how it talks to its plugins. Plugins left unticked or not updated will be refused until they are updated.',
      )
      expect(banner()?.querySelector('[data-update-major-refused]')?.textContent?.trim()).toBe(
        'Refused by the new core until updated: zed',
      )

      // A dependent left unticked: its row says it, the banner names it, and
      // the core row leaves the vaguer "may refuse" to the non-breaking case.
      await click('cd')
      expect(warning('cd')).toBe('Left as it is, it will be refused by the new core until it is updated.')
      expect(banner()?.querySelector('[data-update-major-refused]')?.textContent?.trim()).toBe(
        'Refused by the new core until updated: cd, zed',
      )
      expect(warning('core')).toBeNull()

      // The core unticked: the running core stays, nothing breaks.
      await click('core')
      expect(banner()).toBeNull()
    })

    it('names a plugin whose running binary the new core refuses, though its update is accepted by the running one', async () => {
      // A third party's plugin from its own repository, still built for the
      // old major: its update is accepted by the running core, so nothing
      // about the offered version says it depends on the core. Its binary,
      // left as it is, is refused by the new core all the same.
      mountDialog([
        breakingCore(),
        offer('zed', {
          kind: 'third_party', third_party_repo: 'z/zed',
          with_core: REFUSED, with_running_core: { fit: 'compatible' }, installed_with_core: REFUSED,
        }),
      ])
      await flushPromises()
      expect(isChecked('zed')).toBe('false')
      expect(banner()?.querySelector('[data-update-major-refused]')?.textContent?.trim()).toBe(
        'Refused by the new core until updated: zed',
      )
      // The refusal first, then the source: the row's label is the bare
      // name, and this note is the only place naming the repository.
      // **[MUTATION]** drop the source note from the refused third-party
      // warning: red.
      expect(warning('zed')).toBe(
        'Left as it is, it will be refused by the new core until it is updated. '
        + 'Third-party plugin from z/zed. Never selected automatically.',
      )
      // The warning speaks of the binary kept; the offered version's line
      // would contradict it, so it is not shown.
      expect(fit('zed')).toBeNull()
    })

    it('does not warn about a dependent whose running binary the new core accepts', async () => {
      // Its new version adds the contract the break moves (so the running
      // core refuses that version: a dependent), but the binary installed now
      // does not speak it at all, and the new core accepts it as it is.
      mountDialog([
        breakingCore(),
        offer('cd', { with_core: { fit: 'compatible' }, with_running_core: REFUSED, installed_with_core: { fit: 'compatible' } }),
      ])
      await flushPromises()
      await click('cd')
      expect(isChecked('cd')).toBe('false')
      expect(warning('cd')).toBeNull()
      expect(banner()?.querySelector('[data-update-major-refused]')).toBeNull()
      // The banner itself stays, and claims no list it does not show.
      expect(banner()?.querySelector('p')?.textContent?.trim()).toBe(
        'Major update: the core changes how it talks to its plugins. Plugins left unticked or not updated will be refused until they are updated.',
      )
    })

    it('shows no major banner for a core that does not break the wire', async () => {
      mountDialog([core(), offer('radio', { with_core: { fit: 'compatible' }, with_running_core: { fit: 'compatible' } })])
      await flushPromises()
      expect(isChecked('core')).toBe('true')
      expect(banner()).toBeNull()
    })
  })
})
