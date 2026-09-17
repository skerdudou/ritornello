import { flushPromises, mount } from '@vue/test-utils'
import { SelectItem } from '@ritornello/ui'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { LanguageCompleteness, LocalePayload } from '../types'

// Real phrase keys, with the `{done}`/`{total}`/`{plugins}`/`{remaining}`
// tokens the component actually interpolates — not placeholders, so a wrong
// parameter name would fail this test the same way a wrong parameter would
// fail in production (`{missing}` stays visible rather than becoming empty,
// see the kit's `interpolate`).
const CATALOGUE = {
  language: 'Language',
  locale_completeness_core: 'core /{total}',
  locale_completeness_core_plugin: 'core + 1 plugin /{total}',
  locale_completeness_core_plugins: 'core + {plugins} plugins /{total}',
  locale_completeness_none: '0 /{total}',
  locale_completeness_plugin: '1 plugin /{total}',
  locale_completeness_plugins: '{plugins} plugins /{total}',
  locale_fallback_label: 'Fallback',
  locale_fallback_hint: 'English always underlies every choice — picking it here adds no extra fallback.',
  locale_fallback_result_one: '{done} /{total}, 1 module still in English',
  locale_fallback_result: '{done} /{total}, {remaining} modules still in English',
}

// The brief's own worked example ("cœur + 3 greffons /7", "2 greffons /7"),
// on the seven texted modules task 12's report names: core + cd, files,
// generic-input, mpd, musicbrainz, radio.
const TOTAL = 7
const EN_ALL = ['core', 'cd', 'files', 'generic-input', 'mpd', 'musicbrainz', 'radio']

/** `done` and `complete_modules.length` kept in lockstep, the way the real
 *  server always builds them (both come from the same `Coverage`). */
function entry(language: string, complete: boolean, complete_modules: string[]): LanguageCompleteness {
  return { language, complete, done: complete_modules.length, total: TOTAL, complete_modules }
}

function payload(entries: LanguageCompleteness[], fallback_candidates = ['en', 'fr', 'de']): LocalePayload {
  return {
    locales: entries.map((e) => e.language),
    current: entries[0]?.language ?? null,
    completeness: entries,
    fallback_current: 'en',
    fallback_candidates,
  }
}

const BASE: LocalePayload = payload([
  entry('en', true, EN_ALL),
  // core + 3 plugins /7 — the brief's own first example.
  entry('fr', false, ['core', 'files', 'generic-input', 'mpd']),
  // 2 plugins /7, core not covered — the brief's own second example.
  entry('de', false, ['cd', 'radio']),
])

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
    props: { payload: props.payload ?? BASE, lang: props.lang, fallback: props.fallback },
  })
  await flushPromises()
  return w
}

beforeEach(async () => {
  const { resetCatalog } = await import('../composables/useCatalog')
  resetCatalog()
})

describe('LanguageCard — the own-language annotation, all six combinations', () => {
  it('shows nothing extra for a complete language', async () => {
    const w = await mountCard({ lang: 'en', fallback: 'en' })
    expect(w.find('[data-locale-completeness]').exists()).toBe(false)
    expect(w.find('[data-fallback-row]').exists()).toBe(false)
    expect(w.find('[data-language-select]').text()).toBe('English')
  })

  it('core + several plugins: "core + {plugins} plugins /{total}"', async () => {
    const w = await mountCard({ lang: 'fr', fallback: 'en' })
    expect(w.find('[data-locale-completeness]').text()).toBe('core + 3 plugins /7')
  })

  it('several plugins, core not covered: "{plugins} plugins /{total}"', async () => {
    const w = await mountCard({ lang: 'de', fallback: 'en' })
    expect(w.find('[data-locale-completeness]').text()).toBe('2 plugins /7')
  })

  it('core alone, no plugin covered: "core /{total}"', async () => {
    const p = payload([entry('en', true, EN_ALL), entry('fr', false, ['core'])])
    const w = await mountCard({ lang: 'fr', fallback: 'en', payload: p })
    expect(w.find('[data-locale-completeness]').text()).toBe('core /7')
  })

  it('core + exactly one plugin: "core + 1 plugin /{total}" (singular, not "1 plugins")', async () => {
    const p = payload([entry('en', true, EN_ALL), entry('fr', false, ['core', 'radio'])])
    const w = await mountCard({ lang: 'fr', fallback: 'en', payload: p })
    expect(w.find('[data-locale-completeness]').text()).toBe('core + 1 plugin /7')
  })

  it('exactly one plugin, core not covered: "1 plugin /{total}" (singular)', async () => {
    const p = payload([entry('en', true, EN_ALL), entry('fr', false, ['radio'])])
    const w = await mountCard({ lang: 'fr', fallback: 'en', payload: p })
    expect(w.find('[data-locale-completeness]').text()).toBe('1 plugin /7')
  })

  it('nothing covered at all: "0 /{total}"', async () => {
    const p = payload([entry('en', true, EN_ALL), entry('fr', false, [])])
    const w = await mountCard({ lang: 'fr', fallback: 'en', payload: p })
    expect(w.find('[data-locale-completeness]').text()).toBe('0 /7')
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
})

describe('LanguageCard — the fallback result, a true set union (fix round 1)', () => {
  it('"en" is the wire\'s "no fallback": nothing extra claims a benefit it cannot provide', async () => {
    const w = await mountCard({ lang: 'de', fallback: 'en' })
    expect(w.find('[data-fallback-row]').exists()).toBe(true)
    expect(w.find('[data-fallback-select]').text()).toBe('English')
    expect(w.find('[data-fallback-result]').exists()).toBe(false)
  })

  it('disjoint coverage: the union closes every gap the old Math.max bound could not see', async () => {
    // chosen "de" covers {cd, radio} (2/7), fallback "fr" covers {core,
    // files, generic-input, mpd} (4/7), entirely disjoint from "de"'s set.
    // `Math.max(2, 4) = 4` (the fix round 1 report's own worked example,
    // scaled to this fixture) would have printed "4 /7, 3 still in
    // English"; the true union is 6/7, exactly one module (musicbrainz)
    // genuinely uncovered.
    const w = await mountCard({ lang: 'de', fallback: 'fr' })
    expect(w.find('[data-fallback-result]').text()).toBe('6 /7, 1 module still in English')
  })

  it('overlapping coverage: a set union, not a sum of the two counts', async () => {
    // chosen covers {core, cd} (2), fallback covers {core, files} (2),
    // sharing "core". A naive `chosen.length + fallback.length` would print
    // 4; the true union is 3 — this is the check that would fail against
    // that specific mistake.
    const p = payload([
      entry('en', true, EN_ALL),
      entry('fr', false, ['core', 'cd']),
      entry('de', false, ['core', 'files']),
    ])
    const w = await mountCard({ lang: 'fr', fallback: 'de', payload: p })
    expect(w.find('[data-fallback-result]').text()).toBe('3 /7, 4 modules still in English')
  })

  it('exactly one module remains: the singular key, not "1 modules"', async () => {
    const p = payload([
      entry('en', true, EN_ALL),
      entry('fr', false, ['core', 'cd', 'files', 'generic-input', 'mpd']),
      entry('de', false, ['musicbrainz']),
    ])
    const w = await mountCard({ lang: 'fr', fallback: 'de', payload: p })
    expect(w.find('[data-fallback-result]').text()).toBe('6 /7, 1 module still in English')
  })

  it('shows nothing once the union already closes every gap', async () => {
    const p = payload([
      entry('en', true, EN_ALL),
      entry('fr', false, ['core', 'cd', 'files', 'generic-input', 'mpd']),
      entry('de', false, ['musicbrainz', 'radio']),
    ])
    const w = await mountCard({ lang: 'fr', fallback: 'de', payload: p })
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

describe('LanguageCard — fix round 1 findings 5 and 7', () => {
  it('never offers the chosen language as its own fallback (finding 7/R6)', async () => {
    // "de" is chosen and is itself a `fallback_candidates` entry (a core
    // language can be incomplete too) — the review's own repro: picking it
    // as its own fallback rendered a no-op result as if it had acted.
    //
    // Scoped to `[data-fallback-row]`'s DOM subtree rather than the whole
    // tree: reka-ui's `SelectItem`s for both `<Select>`s on this card are
    // real component-tree children either way, but the *language* select
    // (outside this row) also has a "de" item — comparing option **values**
    // via `.props('value')` (not text) is what tells the two apart without
    // depending on which one happens to teleport where.
    const w = await mountCard({ lang: 'de', fallback: 'en' })
    const values = w.findAllComponents(SelectItem).map((i) => i.props('value'))
    // "de" appears exactly once — from the language select's own union
    // list, which this filter never touches — where an unfiltered
    // `fallback_candidates` would make it appear twice, once per select.
    expect(values.filter((v) => v === 'de')).toHaveLength(1)
    // "fr" is untouched by the filter (it is not the chosen language) and
    // legitimately belongs to both selects: the language union and the
    // fallback candidates.
    expect(values.filter((v) => v === 'fr')).toHaveLength(2)
  })

  it('offers only the core languages as a fallback, never the full union (owner rule 4)', async () => {
    // In `BASE`, `locales` and `fallback_candidates` happen to be the same
    // three codes — a payload built that way cannot tell "the fallback
    // select reads `fallback_candidates`" apart from "it reads `locales`",
    // since both would render the same options. Here `locales` strictly
    // contains a fourth, plugin-only language ("nl") that is genuinely
    // absent from `fallback_candidates` — the review's own repro for
    // swapping `payload.fallback_candidates` for `payload.locales` in the
    // template, which this shape is the only way to catch.
    const p: LocalePayload = {
      ...payload([
        entry('en', true, EN_ALL),
        entry('fr', false, ['core', 'files', 'generic-input', 'mpd']),
        entry('de', false, ['cd', 'radio']),
        entry('nl', false, ['musicbrainz']),
      ]),
      fallback_candidates: ['en', 'fr'],
    }
    const w = await mountCard({ lang: 'de', fallback: 'en', payload: p })
    const values = w.findAllComponents(SelectItem).map((i) => i.props('value'))
    // "nl" is in the language select's union (once) but must never reach
    // the fallback select — an unfiltered `payload.locales` would make it
    // appear a second time.
    expect(values.filter((v) => v === 'nl')).toHaveLength(1)
    // "de" (the chosen language, also excluded per finding 7/R6) and "nl"
    // both absent from the fallback options, leaving exactly "en" and "fr".
    expect(values.filter((v) => v === 'de')).toHaveLength(1)
  })

  it('explains what picking "English" as a fallback means (finding 5/R7)', async () => {
    const w = await mountCard({ lang: 'de', fallback: 'en' })
    expect(w.find('[data-fallback-hint]').exists()).toBe(true)
    expect(w.find('[data-fallback-hint]').text().length).toBeGreaterThan(0)
  })
})
