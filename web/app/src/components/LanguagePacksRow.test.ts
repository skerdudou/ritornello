import { mount, flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { Select } from '@ritornello/ui'
import type { LanguageBusy, LanguagePackRow, LocalePayload } from '../types'
import { packRow } from '../testing/languagePacks'

const CATALOGUE = {
  language_pack_install: 'Install',
  language_pack_update: 'Update',
  language_pack_remove: 'Remove',
  language_pack_update_available: 'A newer pack is available',
  language_pack_installing: 'Installing {language}…',
  language_pack_removing: 'Removing {language}…',
  language_pack_official: 'Ritornello (official pack)',
  language_pack_from: 'From {source}',
  language_pack_not_installed: 'Not installed',
  language_pack_overlap_intro: 'Several packs cover the same module.',
  language_pack_overlap_module: 'Module {module}',
  language_pack_overlap_choose: 'Pack that speaks for {module}',
}

const THIRD = 'ritornello-xlang-fr-0123456789ab'
const OFFICIAL = 'ritornello-lang-fr'

/** `fr`: the official pack current, a third party's pack installed too. */
function frWithThirdParty(extra: Partial<LanguagePackRow> = {}): LanguagePackRow {
  return packRow('fr', '0.2.1', '0.2.1', {
    packs: [
      { id: OFFICIAL, source: null, installed: '0.2.1', offered: '0.2.1' },
      { id: THIRD, source: 'someone/fr-extra', installed: '1.0.0', offered: '1.0.0' },
    ],
    ...extra,
  })
}

function payloadOf(...packs: LanguagePackRow[]): LocalePayload {
  return { ...BASE, packs }
}

const BASE: LocalePayload = {
  locales: ['en', 'fr'],
  current: 'fr',
  completeness: [],
  fallback_current: 'en',
  fallback_candidates: ['en'],
  packs: [
    packRow('fr', '0.2.1', '0.2.1'),
    packRow('de', null, '0.2.1'),
    packRow('es', '0.2.0', '0.2.1'),
  ],
}

async function mountRow(payload: LocalePayload = BASE, busy: LanguageBusy | null = null) {
  vi.stubGlobal('fetch', vi.fn(async (url: string) => {
    if (url === '/api/i18n') return new Response(JSON.stringify(CATALOGUE), { status: 200 })
    return new Response('unknown', { status: 404 })
  }))
  const { useCatalog } = await import('../composables/useCatalog')
  const LanguagePacksRow = (await import('./LanguagePacksRow.vue')).default
  await useCatalog().reload()
  const w = mount(LanguagePacksRow, { props: { payload, busy } })
  await flushPromises()
  return w
}

beforeEach(async () => {
  const { resetCatalog } = await import('../composables/useCatalog')
  resetCatalog()
})

describe('LanguagePacksRow', () => {
  // Follow-up B: a pack that is only offered moved to `AddLanguageDialog`.
  // Both halves: the installed rows are all there, and the offered-only one
  // is nowhere in this list — a mutant that filters nothing, or hides
  // everything, fails one of the two.
  it('lists only the packs on disk, with remove, and never a pack that is only offered', async () => {
    const w = await mountRow()
    expect(w.findAll('[data-pack-row]').map((r) => r.find('[data-pack-remove]').attributes('data-pack-remove')))
      .toEqual(['fr', 'es'])
    expect(w.find('[data-pack-remove="fr"]').exists()).toBe(true)
    expect(w.find('[data-pack-remove="es"]').exists()).toBe(true)
    expect(w.text()).not.toContain('Deutsch')
    expect(w.find('[data-pack-remove="de"]').exists()).toBe(false)
    expect(w.find('[data-pack-install="de"]').exists()).toBe(false)
    expect(w.find('[data-pack-install]').exists()).toBe(false)
  })

  it('shows nothing when every pack is only offered', async () => {
    const w = await mountRow({ ...BASE, packs: [packRow('de', null, '0.2.1')] })
    expect(w.find('[data-language-packs]').exists()).toBe(false)
  })

  it('offers update, and says so, only when the offered version differs', async () => {
    const w = await mountRow()
    expect(w.find('[data-pack-update="es"]').exists()).toBe(true)
    expect(w.find('[data-pack-update="fr"]').exists()).toBe(false)
  })

  /// Nothing is shown for a language nobody publishes a pack for and the
  /// device does not have: the card's arbitrated rule is that an annotation
  /// appears only where it carries news.
  it('shows nothing at all when no pack is offered or installed', async () => {
    const w = await mountRow({ ...BASE, packs: [] })
    expect(w.find('[data-language-packs]').exists()).toBe(false)
  })

  it('emits the language, and never calls the API itself', async () => {
    const w = await mountRow()
    await w.find('[data-pack-update="es"]').trigger('click')
    expect(w.emitted('install')).toEqual([['es']])
    await w.find('[data-pack-remove="fr"]').trigger('click')
    expect(w.emitted('remove')).toEqual([['fr']])
  })

  /// A gesture in flight disables every button of that row rather than only
  /// the one clicked: two installs racing would both be enqueued, and the
  /// queue holds four.
  it('disables the row whose gesture is in flight', async () => {
    const w = await mountRow(BASE, { language: 'es', action: 'install' })
    expect(w.find('[data-pack-update="es"]').attributes('disabled')).toBeDefined()
    expect(w.find('[data-pack-remove="es"]').attributes('disabled')).toBeDefined()
    expect(w.find('[data-pack-remove="fr"]').attributes('disabled')).toBeUndefined()
  })

  it('labels the busy row with the verb `busy.action` names', async () => {
    const remove = await mountRow(BASE, { language: 'fr', action: 'remove' })
    expect(remove.find('[data-pack-busy]').text()).toContain('Removing')

    const install = await mountRow(BASE, { language: 'es', action: 'install' })
    expect(install.find('[data-pack-busy]').text()).toContain('Installing')
  })

  /// Fix round 1, finding 3: this is the case that disproves inferring the
  /// verb from `row.installed`, and it is not hypothetical — it is the exact
  /// state a real "Update" click produces. `fr` is *installed*
  /// (`installed: '0.2.1'`), so a component that guessed "installed →
  /// removing" would print "Removing" here even though ConfigView explicitly
  /// says this is an install (a reinstall over the existing pack, the
  /// gesture "Update" performs). The component must render the verb it is
  /// given, not the one `row.installed` would suggest.
  it('names "installing" for a reinstall over an already-installed pack, never "removing"', async () => {
    const w = await mountRow(BASE, { language: 'fr', action: 'install' })
    expect(w.find('[data-pack-busy]').text()).toContain('Installing')
    expect(w.find('[data-pack-busy]').text()).not.toContain('Removing')
  })
})

describe('LanguagePacksRow: several packs of one language', () => {
  it("names each pack's source under a language that has more than one, official as such", async () => {
    const w = await mountRow(payloadOf(frWithThirdParty()))
    const lines = w.findAll('[data-pack-source]').map((l) => [l.attributes('data-pack-source'), l.text()])
    expect(lines).toEqual([
      [OFFICIAL, 'Ritornello (official pack) — 0.2.1'],
      [THIRD, 'From someone/fr-extra — 1.0.0'],
    ])
  })

  it('renders a single-pack language exactly as before: no source lines, no overlap block', async () => {
    const w = await mountRow()
    expect(w.find('[data-pack-sources]').exists()).toBe(false)
    expect(w.find('[data-pack-overlaps]').exists()).toBe(false)
  })

  // The regression this guards: Update decided on the official pack alone
  // (`offered !== installed`) hides a third party's news when ours is current.
  it('offers Update when only a third-party pack has one, the official pack being current', async () => {
    const row = frWithThirdParty({
      update_available: true,
      packs: [
        { id: OFFICIAL, source: null, installed: '0.2.1', offered: '0.2.1' },
        { id: THIRD, source: 'someone/fr-extra', installed: '1.0.0', offered: '1.1.0' },
      ],
    })
    const w = await mountRow(payloadOf(row))
    expect(w.find('[data-pack-update="fr"]').exists()).toBe(true)
    expect(w.find('[data-pack-update-note]').exists()).toBe(true)
  })

  it('does not offer Update when no pack has news', async () => {
    const w = await mountRow(payloadOf(frWithThirdParty()))
    expect(w.find('[data-pack-update="fr"]').exists()).toBe(false)
  })

  // P11: a filter on the official pack alone dropped this language from the
  // card, leaving a third-party pack nothing to remove or update it with.
  it("keeps a language whose only installed pack is a third party's", async () => {
    const row = packRow('fr', null, '0.2.1', {
      update_available: false,
      packs: [
        { id: OFFICIAL, source: null, installed: null, offered: '0.2.1' },
        { id: THIRD, source: 'someone/fr-extra', installed: '1.0.0', offered: '1.0.0' },
      ],
    })
    const w = await mountRow(payloadOf(row))
    expect(w.find('[data-pack-remove="fr"]').exists()).toBe(true)
  })

  it('shows no overlap block when no module is shared', async () => {
    const w = await mountRow(payloadOf(frWithThirdParty({ overlaps: [] })))
    expect(w.find('[data-pack-overlaps]').exists()).toBe(false)
    expect(w.findComponent(Select).exists()).toBe(false)
  })

  const overlaps = [{ module: 'core', packs: [THIRD, OFFICIAL], active: THIRD }]

  it('shows which pack speaks for an overlapping module, right at mount', async () => {
    const w = await mountRow(payloadOf(frWithThirdParty({ overlaps })))
    expect(w.find('[data-pack-overlaps]').exists()).toBe(true)
    expect(w.get('[data-pack-overlap="core"]').text()).toContain('Module core')
    expect(w.findComponent(Select).props('modelValue')).toBe(THIRD)
    // The label is the pack's source, not a raw id nor a catalog key.
    expect(w.get('[data-pack-preference="core"]').text()).toBe('From someone/fr-extra')
  })

  it('emits the preferred pack for the module when the operator picks another', async () => {
    const w = await mountRow(payloadOf(frWithThirdParty({ overlaps })))
    await w.findComponent(Select).vm.$emit('update:modelValue', OFFICIAL)
    expect(w.emitted('prefer')).toEqual([['fr', 'core', OFFICIAL]])
  })

  it('follows the core after a reload: the label changes with the active pack', async () => {
    const w = await mountRow(payloadOf(frWithThirdParty({ overlaps })))
    await w.setProps({
      payload: payloadOf(frWithThirdParty({
        overlaps: [{ ...overlaps[0]!, packs: [OFFICIAL, THIRD], active: OFFICIAL }],
      })),
    })
    expect(w.get('[data-pack-preference="core"]').text()).toBe('Ritornello (official pack)')
  })

  it('shows a refused preference next to the select it was made on, and nowhere else', async () => {
    const w = await mountRow(payloadOf(frWithThirdParty({
      overlaps: [...overlaps, { module: 'radio', packs: [THIRD, OFFICIAL], active: THIRD }],
    })))
    await w.setProps({ preferenceError: { language: 'fr', module: 'radio', message: 'not installed' } })
    expect(w.get('[data-pack-overlap="radio"] [data-pack-preference-error]').text()).toBe('not installed')
    expect(w.find('[data-pack-overlap="core"] [data-pack-preference-error]').exists()).toBe(false)
  })
})
