<script setup lang="ts">
/**
 * The language selector and its fallback control, extracted out of
 * `ConfigView.vue`'s "Language and display" card so the display rules
 * (task 14 of the language-packs chantier) can be pinned by a component
 * test rather than only by the page's own giant journey.
 *
 * This component only **renders**: it holds no `ref` of its own for the
 * chosen language or the fallback, and makes no API call. Both are
 * `v-model`s the parent (`ConfigView.vue`) owns, because the actual `PUT
 * /api/locale` is submitted together with the rest of the "Language and
 * display" card behind one button (`saveDisplay`) — see that function's own
 * doc for why the two routes must not fire independently.
 */
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@ritornello/ui'
import { computed } from 'vue'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import type { LocalePayload } from '../types'

const props = defineProps<{
  payload: LocalePayload
  lang: string
  fallback: string
}>()
const emit = defineEmits<{ 'update:lang': [string]; 'update:fallback': [string] }>()

const { t } = useCatalog()

function completenessOf(code: string) {
  return props.payload.completeness.find((c) => c.language === code)
}

/**
 * Splits a list of complete module names into "is `core` among them" and
 * "how many others" — the decomposition the brief's own picture names
 * ("cœur + 3 greffons /7"): `core` is singled out by name because it is the
 * one module every device has, `plugins` counts everything else without
 * naming them individually.
 */
function moduleSplit(names: readonly string[]): { core: boolean; plugins: number } {
  const core = names.includes('core')
  return { core, plugins: names.length - (core ? 1 : 0) }
}

/**
 * The annotation for one language line. Empty for a complete language (and
 * for a code the payload has no completeness entry for, which a caller
 * should treat exactly like "nothing to say" rather than crash on) — **the
 * owner's rule: nothing is displayed for a complete language, just its
 * name.** Six whole-sentence catalog keys, named parameters only, never a
 * concatenation — the trap this chantier has already paid for once. Six
 * and not one because the brief names the unit and singles out `core`
 * ("cœur + 3 greffons /7", "2 greffons /7"): the six keys are every
 * combination of "is `core` among the complete modules" × "how many others,
 * bucketed 0 / 1 / many" (a bare count needs the singular/plural split this
 * codebase already uses for `update_out_of_step`/`update_out_of_step_one`,
 * so "1 greffon" never collides with the "0 greffons"/"{n} greffons" forms
 * that would need a French verb or adjective to agree in number).
 */
function annotation(code: string): string {
  const c = completenessOf(code)
  if (!c || c.complete) return ''
  const { core, plugins } = moduleSplit(c.complete_modules)
  const total = c.total
  if (core && plugins === 0) return t.value('locale_completeness_core', { total })
  if (core && plugins === 1) return t.value('locale_completeness_core_plugin', { total })
  if (core) return t.value('locale_completeness_core_plugins', { plugins, total })
  if (plugins === 0) return t.value('locale_completeness_none', { total })
  if (plugins === 1) return t.value('locale_completeness_plugin', { total })
  return t.value('locale_completeness_plugins', { plugins, total })
}

const languageLabel = computed(() => (props.lang ? languageName(props.lang) : ''))
const chosenAnnotation = computed(() => annotation(props.lang))

/**
 * The fallback control appears **only** when the chosen language is
 * incomplete (owner's rule 3: a complete language has no holes, so a
 * fallback for it could never fire). Deliberately keyed off `payload`, not
 * off `fallback` itself: hiding this control must never clear the stored
 * `fallback` value (owner's rule: "its value stays memorized once the
 * control disappears" — a device that goes back to an incomplete language
 * later must not have to re-choose).
 */
const showFallback = computed(() => {
  const c = completenessOf(props.lang)
  return !!c && !c.complete
})

/**
 * The candidates offered, **minus the chosen language itself** (fix round
 * 1, task 14 review, finding 7/R6). `fallback_candidates` is core-language
 * only, and the chosen language can legitimately be one of them (a core
 * language can be incomplete too) — offering it as its own fallback let the
 * card render a no-op ("Français", chosen and offered as its own repli) as
 * if it had acted. If this ever leaves the list empty the honest answer is
 * `["en"]`: the wire guarantees `"en"` is always among `fallback_candidates`
 * (`LocaleResponse::fallback_candidates`'s own doc), and English is never
 * the chosen language while this control shows (English is always
 * `complete`, so `showFallback` would already be `false`) — meaning "en"
 * can never be the value filtered out here.
 */
const fallbackCandidates = computed(() => props.payload.fallback_candidates.filter((c) => c !== props.lang))

const fallbackLabel = computed(() => (props.fallback ? languageName(props.fallback) : ''))

/**
 * What still falls to English once the chosen fallback is applied — "the
 * only way to know if the fallback served" (the plan's own words). `"en"`
 * is the wire's representable "no fallback" (never `null`, never `""` — see
 * `LocaleRequest.fallback`'s own doc on the Rust side): picking it changes
 * nothing over the chosen language alone, so this line stays empty rather
 * than repeating `chosenAnnotation` right below it.
 *
 * **A true set union, not a bound.** `complete_modules` (fix round 1, task
 * 14 review, finding 1/R1) names which modules each language actually
 * covers, so `done` here is `|chosen.complete_modules ∪ fallback
 * .complete_modules|` — the real answer, not `Math.max(chosen.done,
 * fallback.done)`, which the review measured as reachable up to "3 still in
 * English" when the true remainder was 0 (chosen and fallback covering
 * disjoint module sets — the case this feature exists for, since
 * `fallback_candidates` is deliberately the core's own languages while the
 * chosen language may come from a single plugin's pack). The server
 * already builds this list in the same handler and the same registry read
 * that produces `done`/`total`; publishing it cost one additive field, no
 * new route.
 *
 * **The unit is named** ("modules", per the owner's ruling — the remaining
 * count is not decomposed by `core`/plugin the way the own-language
 * annotation is, since a module set can span both and "N greffons" would
 * misname `core` if it were the one still uncovered) with the same
 * singular/plural key split as `annotation` uses, for the same reason.
 *
 * **`remaining === 0` renders a positive line, never silence** (fix round
 * 2, task 14 re-review, finding C). Before this, a fallback that closed
 * every gap suppressed the line entirely — indistinguishable, on screen,
 * from "no fallback effect at all" (the exact same rendering `fallback ===
 * 'en'` produces just above). The brief names this line's whole purpose as
 * "the only way to know if the fallback served"; silence in the one case
 * where it demonstrably did serve defeats that purpose, so success gets its
 * own key rather than reusing the empty string that also means "nothing to
 * report yet".
 */
const fallbackAnnotation = computed(() => {
  if (!showFallback.value || props.fallback === 'en') return ''
  const chosen = completenessOf(props.lang)
  const fb = completenessOf(props.fallback)
  if (!chosen || !fb) return ''
  const total = chosen.total
  const done = new Set([...chosen.complete_modules, ...fb.complete_modules]).size
  const remaining = total - done
  if (remaining <= 0) return t.value('locale_fallback_result_none', { done, total })
  if (remaining === 1) return t.value('locale_fallback_result_one', { done, total })
  return t.value('locale_fallback_result', { done, total, remaining })
})
</script>

<template>
  <div class="flex flex-col gap-2">
    <div class="flex flex-wrap items-center gap-2">
      <Select :model-value="lang" @update:model-value="(v) => emit('update:lang', String(v))">
        <SelectTrigger class="min-w-32" data-language-select :aria-label="t('language')"><SelectValue>{{ languageLabel }}</SelectValue></SelectTrigger>
        <SelectContent>
          <!-- Name of the language and not its code: "français" is read,
               "fr" is guessed. The code remains the value sent to the core. -->
          <SelectItem v-for="l in payload.locales" :key="l" :value="l">
            <div class="flex flex-col items-start">
              <span>{{ languageName(l) }}</span>
              <span v-if="annotation(l)" class="text-xs text-muted-foreground" data-locale-annotation>{{ annotation(l) }}</span>
            </div>
          </SelectItem>
        </SelectContent>
      </Select>
      <span v-if="chosenAnnotation" class="text-sm text-muted-foreground" data-locale-completeness>{{ chosenAnnotation }}</span>
    </div>

    <div v-if="showFallback" class="flex flex-col gap-1" data-fallback-row>
      <div class="flex flex-wrap items-center gap-2">
        <label class="grid gap-1 text-sm">
          {{ t('locale_fallback_label') }}
          <Select :model-value="fallback" @update:model-value="(v) => emit('update:fallback', String(v))">
            <SelectTrigger class="min-w-32" data-fallback-select :aria-label="t('locale_fallback_label')"><SelectValue>{{ fallbackLabel }}</SelectValue></SelectTrigger>
            <SelectContent>
              <!-- Readable name as primary, bare code as secondary — the
                   same pattern the language select and the audio-device
                   select already use (fix round 2, task 14 re-review,
                   finding D: a bare-label item let a dropped `<SelectValue>`
                   override go unguarded on this select specifically, since
                   there was nothing beyond the label for the default to
                   leak). -->
              <SelectItem v-for="c in fallbackCandidates" :key="c" :value="c">
                <div class="flex flex-col items-start">
                  <span>{{ languageName(c) }}</span>
                  <span class="text-xs text-muted-foreground">{{ c }}</span>
                </div>
              </SelectItem>
            </SelectContent>
          </Select>
        </label>
        <span v-if="fallbackAnnotation" class="text-sm text-muted-foreground" data-fallback-result>{{ fallbackAnnotation }}</span>
      </div>
      <!-- The owner's rule: English is a non-removable third level, never
           itself presented as removable — but sitting in this list as an
           ordinary pick, nothing said that picking it here means "no
           fallback", or that English remains underneath regardless (fix
           round 1, task 14 review, finding 5/R7). -->
      <p class="text-xs text-muted-foreground" data-fallback-hint>{{ t('locale_fallback_hint') }}</p>
    </div>
  </div>
</template>
