import { mount, flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import type { LocalePayload } from '../types'

const CATALOGUE = {
  language_pack_install: 'Install',
  language_pack_update: 'Update',
  language_pack_remove: 'Remove',
  language_pack_update_available: 'A newer pack is available',
  language_pack_installing: 'Installing {language}…',
  language_pack_removing: 'Removing {language}…',
}

const BASE: LocalePayload = {
  locales: ['en', 'fr'],
  current: 'fr',
  completeness: [],
  fallback_current: 'en',
  fallback_candidates: ['en'],
  packs: [
    { language: 'fr', installed: '0.2.1', offered: '0.2.1' },
    { language: 'de', installed: null, offered: '0.2.1' },
    { language: 'es', installed: '0.2.0', offered: '0.2.1' },
  ],
}

async function mountRow(payload: LocalePayload = BASE, busy: string | null = null) {
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
  it('offers install for a language the device does not have, and remove for one it has', async () => {
    const w = await mountRow()
    expect(w.find('[data-pack-install="de"]').exists()).toBe(true)
    expect(w.find('[data-pack-remove="de"]').exists()).toBe(false)
    expect(w.find('[data-pack-remove="fr"]').exists()).toBe(true)
    expect(w.find('[data-pack-install="fr"]').exists()).toBe(false)
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
    await w.find('[data-pack-install="de"]').trigger('click')
    expect(w.emitted('install')).toEqual([['de']])
    await w.find('[data-pack-remove="fr"]').trigger('click')
    expect(w.emitted('remove')).toEqual([['fr']])
  })

  /// A gesture in flight disables every button of that row rather than only
  /// the one clicked: two installs racing would both be enqueued, and the
  /// queue holds four.
  it('disables the row whose gesture is in flight', async () => {
    const w = await mountRow(BASE, 'de')
    expect(w.find('[data-pack-install="de"]').attributes('disabled')).toBeDefined()
    expect(w.find('[data-pack-remove="fr"]').attributes('disabled')).toBeUndefined()
  })

  /// `busy` alone is just a language code: `fr` is installed, so it licenses
  /// both "Update" and "Remove", and the two disagree on which sentence
  /// belongs on screen. Pinned against a component that infers the sentence
  /// from `row.installed` alone (always "removing" once a pack is on disk)
  /// rather than from the button actually pressed.
  it('labels the busy row installing or removing, matching the button actually pressed', async () => {
    const w = await mountRow(BASE, null)
    await w.find('[data-pack-remove="fr"]').trigger('click')
    await w.setProps({ busy: 'fr' })
    expect(w.find('[data-pack-busy]').text()).toContain('Removing')

    const w2 = await mountRow(BASE, null)
    await w2.find('[data-pack-update="es"]').trigger('click')
    await w2.setProps({ busy: 'es' })
    expect(w2.find('[data-pack-busy]').text()).toContain('Installing')
  })
})
