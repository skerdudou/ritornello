import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { LocalePayload } from '../types'

// Real phrase keys, with the `{done}`/`{total}`/`{remaining}` tokens the
// component actually interpolates — not placeholders, so a wrong parameter
// name would fail this test the same way a wrong parameter would fail in
// production (`{missing}` stays visible rather than becoming empty, see the
// kit's `interpolate`).
const CATALOGUE = {
  language: 'Language',
  locale_completeness: '{done} of {total} translated',
  locale_fallback_label: 'Fallback',
  locale_fallback_result: '{done} of {total}, {remaining} still in English',
}

// English complete (always, per `ritornello_i18n::coverage`'s own floor
// rule); Français and Deutsch both incomplete, with **different** `done`
// counts on either side of each other — needed so a test can tell a
// `Math.max(chosen, fallback)` implementation apart from one that silently
// always picks one side (see the two "still in English" tests below).
const PAYLOAD: LocalePayload = {
  locales: ['en', 'fr', 'de'],
  current: 'de',
  completeness: [
    { language: 'en', complete: true, done: 7, total: 7 },
    { language: 'fr', complete: false, done: 6, total: 7 },
    { language: 'de', complete: false, done: 3, total: 7 },
  ],
  fallback_current: 'en',
  fallback_candidates: ['en', 'fr'],
}

async function mountCard(props: { lang: string; fallback: string; payload?: LocalePayload }) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOGUE), { status: 200 })
      return new Response('unknown', { status: 404 })
    }),
  )
  const { useCatalog } = await import('../composables/useCatalog')
  const LanguageCard = (await import('./LanguageCard.vue')).default
  await useCatalog().reload()
  const w = mount(LanguageCard, {
    props: { payload: props.payload ?? PAYLOAD, lang: props.lang, fallback: props.fallback },
  })
  await flushPromises()
  return w
}

beforeEach(async () => {
  const { resetCatalog } = await import('../composables/useCatalog')
  resetCatalog()
})

describe('LanguageCard', () => {
  it('shows nothing extra for a complete language', async () => {
    const w = await mountCard({ lang: 'en', fallback: 'en' })
    expect(w.find('[data-locale-completeness]').exists()).toBe(false)
    expect(w.find('[data-fallback-row]').exists()).toBe(false)
    expect(w.find('[data-language-select]').text()).toBe('English')
  })

  it('annotates an incomplete language and shows the fallback control', async () => {
    const w = await mountCard({ lang: 'de', fallback: 'en' })
    expect(w.find('[data-locale-completeness]').text()).toBe('3 of 7 translated')
    expect(w.find('[data-fallback-row]').exists()).toBe(true)
    expect(w.find('[data-fallback-select]').text()).toBe('English')
    // "en" is the wire's "no fallback": nothing extra should claim a
    // benefit that picking "no fallback" cannot provide.
    expect(w.find('[data-fallback-result]').exists()).toBe(false)
  })

  it('an unrecognised language code is treated as complete, not a crash', async () => {
    // `completeness` has no entry for "es" here — the safe default the
    // component's own doc promises for a code the payload says nothing
    // about, so a device offered a language nobody has measured yet still
    // renders instead of throwing.
    const w = await mountCard({ lang: 'es', fallback: 'en' })
    expect(w.find('[data-locale-completeness]').exists()).toBe(false)
    expect(w.find('[data-fallback-row]').exists()).toBe(false)
  })

  it('a real fallback (fallback more complete than the chosen language) reports what remains in English', async () => {
    // chosen = de (done 3), fallback = fr (done 6): the true combined count
    // is unknown (no per-module data on the wire), but `Math.max(3, 6) = 6`
    // is the number this component is documented to show.
    const w = await mountCard({ lang: 'de', fallback: 'fr' })
    expect(w.find('[data-fallback-result]').text()).toBe('6 of 7, 1 still in English')
  })

  it('picks the CHOSEN language’s own count when it is the larger one — proves max(), not "always the fallback"', async () => {
    // Mutation check for the other operand of `Math.max`: a payload where
    // the *chosen* language is more complete than the candidate fallback.
    // An implementation that always used `fallback.done` (dropping the
    // `chosen.done` operand) would print "4 of 7, 3 still in English" here
    // instead — this is exactly the buggy branch that check must catch.
    const payload: LocalePayload = {
      ...PAYLOAD,
      completeness: [
        { language: 'en', complete: true, done: 7, total: 7 },
        { language: 'fr', complete: false, done: 4, total: 7 },
        { language: 'de', complete: false, done: 5, total: 7 },
      ],
    }
    const w = await mountCard({ lang: 'de', fallback: 'fr', payload })
    expect(w.find('[data-fallback-result]').text()).toBe('5 of 7, 2 still in English')
  })

  it('shows nothing once the fallback alone already closes every gap', async () => {
    const payload: LocalePayload = {
      ...PAYLOAD,
      completeness: [
        { language: 'en', complete: true, done: 7, total: 7 },
        { language: 'fr', complete: false, done: 7, total: 7 },
        { language: 'de', complete: false, done: 3, total: 7 },
      ],
    }
    const w = await mountCard({ lang: 'de', fallback: 'fr', payload })
    expect(w.find('[data-fallback-result]').exists()).toBe(false)
  })

  it('the fallback control disappears on a complete language, but never resets the stored value', async () => {
    const w = await mountCard({ lang: 'de', fallback: 'fr' })
    expect(w.find('[data-fallback-row]').exists()).toBe(true)
    expect(w.find('[data-fallback-select]').text()).toBe('Français')

    // Switching to a complete language hides the control — the component
    // itself never owns `fallback`, so nothing it does can clear it; this
    // is the guard against an implementation that "helpfully" emits a
    // reset when the control is hidden.
    await w.setProps({ lang: 'en' })
    expect(w.find('[data-fallback-row]').exists()).toBe(false)
    expect(w.emitted('update:fallback')).toBeUndefined()

    // Back to the original incomplete language, same fallback prop
    // (ConfigView never touched it while the control was hidden): the
    // control reappears showing the very same choice.
    await w.setProps({ lang: 'de' })
    expect(w.find('[data-fallback-row]').exists()).toBe(true)
    expect(w.find('[data-fallback-select]').text()).toBe('Français')
    expect(w.emitted('update:fallback')).toBeUndefined()
  })
})
