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
 * The annotation for one language line. Empty for a complete language (and
 * for a code the payload has no completeness entry for, which a caller
 * should treat exactly like "nothing to say" rather than crash on) — **the
 * owner's rule: nothing is displayed for a complete language, just its
 * name.** A whole-sentence catalog key with named `{done}`/`{total}`
 * parameters, never a concatenation — the trap this chantier has already
 * paid for once (`docs/`'s own account of "cœur + 3 greffons sur 7").
 */
function annotation(code: string): string {
  const c = completenessOf(code)
  if (!c || c.complete) return ''
  return t.value('locale_completeness', { done: c.done, total: c.total })
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

const fallbackLabel = computed(() => (props.fallback ? languageName(props.fallback) : ''))

/**
 * What still falls to English once the chosen fallback is applied — "the
 * only way to know if the fallback served" (the plan's own words). `"en"`
 * is the wire's representable "no fallback" (never `null`, never `""` — see
 * `LocaleRequest.fallback`'s own doc on the Rust side): picking it changes
 * nothing over the chosen language alone, so this line stays empty rather
 * than repeating `chosenAnnotation` right below it.
 *
 * **Why `Math.max`, not a true set union.** The wire (`GET /api/locale`,
 * task 12) serves one `done`/`total` pair *per language*, never a
 * per-module breakdown — `Coverage::modules()` exists on the Rust side but
 * was deliberately not put on the wire (task 12's own report: the
 * `LanguageCompleteness` struct carries only the aggregate numbers "task
 * 14's phrase key needs"). Recovering the true combined count (a module
 * resolved by *either* language) would need that per-module detail, and
 * fetching it would mean a second HTTP round trip per keystroke in the
 * fallback `Select` — exactly the kind of new IPC/route this chantier's own
 * constraints forbid re-introducing. `Math.max(chosenDone, fallbackDone)`
 * is the best lower bound the two aggregate numbers alone support
 * (`|A ∪ B| >= max(|A|, |B|)` always holds), so the displayed "still in
 * English" count is a safe upper bound on the truth — it can overstate how
 * much still needs English, never understate it, which is the direction
 * that does not mislead an owner into over-trusting a fallback.
 */
const fallbackAnnotation = computed(() => {
  if (!showFallback.value || props.fallback === 'en') return ''
  const chosen = completenessOf(props.lang)
  const fb = completenessOf(props.fallback)
  if (!chosen || !fb) return ''
  const total = chosen.total
  const done = Math.max(chosen.done, fb.done)
  const remaining = total - done
  if (remaining <= 0) return ''
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

    <div v-if="showFallback" class="flex flex-wrap items-center gap-2" data-fallback-row>
      <label class="grid gap-1 text-sm">
        {{ t('locale_fallback_label') }}
        <Select :model-value="fallback" @update:model-value="(v) => emit('update:fallback', String(v))">
          <SelectTrigger class="min-w-32" data-fallback-select :aria-label="t('locale_fallback_label')"><SelectValue>{{ fallbackLabel }}</SelectValue></SelectTrigger>
          <SelectContent>
            <SelectItem v-for="c in payload.fallback_candidates" :key="c" :value="c">
              {{ languageName(c) }}
            </SelectItem>
          </SelectContent>
        </Select>
      </label>
      <span v-if="fallbackAnnotation" class="text-sm text-muted-foreground" data-fallback-result>{{ fallbackAnnotation }}</span>
    </div>
  </div>
</template>
