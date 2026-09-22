<script setup lang="ts">
/**
 * One line per language a pack is installed or offered for, sitting right
 * under `LanguageCard`'s selector: the language chooser used to only
 * *announce* what was missing (`LanguageCard`'s completeness annotation, "2
 * greffons /7") — this row is the remedy for it, one gesture per language,
 * everything published for it at once, never plugin by plugin.
 *
 * A pure render component, exactly like `LanguageCard`: props in, events
 * out, **no API call here**. `ConfigView.vue` owns `POST`/`DELETE
 * /api/languages/{language}` and the confirmation dialog in front of
 * "Remove" — this component only ever emits the language code.
 *
 * **The card's arbitrated rule**: nothing is shown where there is no news.
 * A language with no pack offered and none installed renders no line at
 * all — the same convention `LanguageCard`'s own `annotation()` uses for a
 * complete language.
 */
import { Button } from '@ritornello/ui'
import { computed } from 'vue'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import type { LanguageBusy, LanguagePackRow as PackRow, LocalePayload } from '../types'

const props = defineProps<{
  payload: LocalePayload
  /**
   * The gesture `ConfigView` currently has enqueued — the language **and**
   * which of "install" or "remove" it started — or `null` when nothing is in
   * flight. Fix round 1, finding 3: this component used to receive just the
   * busy language and guess the verb from `row.installed` (installed →
   * "removing", not installed → "installing"). That guess is wrong today,
   * not only in some future refactor: a row that licenses both "Update" and
   * "Remove" (a pack already installed, with a newer one offered) keeps
   * `row.installed` non-null while an **Update** is in flight, so the old
   * guess said "removing" for an install. `ConfigView` already knows which
   * button started the request; this component renders exactly that and
   * invents nothing.
   */
  busy: LanguageBusy | null
}>()
const emit = defineEmits<{ install: [string]; remove: [string] }>()

const { t } = useCatalog()

/**
 * The rows worth a line. `installed`/`offered` both `null` never actually
 * reaches `LocalePayload.packs` (`status::locales::language_pack_rows`
 * builds a row from one or the other), but filtering defensively is what the
 * brief's own arbitrated rule ("nothing shown where there is no news") is
 * stated as, and it is what keeps this component correct even if a future
 * caller ever hands it a looser payload.
 */
const rows = computed<PackRow[]>(() =>
  props.payload.packs.filter((p) => p.installed !== null || p.offered !== null),
)

/** A pack is on disk, and the release currently offers a different version. */
function updateAvailable(row: PackRow): boolean {
  return row.installed !== null && row.offered !== null && row.offered !== row.installed
}

/** Whether `row` is the one `busy` names. */
function isBusy(row: PackRow): boolean {
  return props.busy !== null && props.busy.language === row.language
}

/** The verb `busy.action` names for the busy row — never guessed from
 * `row.installed`, see the prop's own doc. */
function busyLabel(row: PackRow): string {
  const language = languageName(row.language)
  return props.busy?.action === 'remove'
    ? t.value('language_pack_removing', { language })
    : t.value('language_pack_installing', { language })
}
</script>

<template>
  <div v-if="rows.length > 0" class="flex flex-col gap-2 border-t border-border pt-4" data-language-packs>
    <div
      v-for="row in rows"
      :key="row.language"
      class="flex flex-wrap items-center gap-2 text-sm"
      data-pack-row
    >
      <span class="min-w-24">{{ languageName(row.language) }}</span>
      <span v-if="updateAvailable(row)" class="text-xs text-muted-foreground" data-pack-update-note>
        {{ t('language_pack_update_available') }}
      </span>
      <span v-if="isBusy(row)" class="text-xs text-muted-foreground" data-pack-busy>
        {{ busyLabel(row) }}
      </span>
      <!-- Only a language with nothing on disk yet can be installed. -->
      <Button
        v-if="row.installed === null"
        variant="outline" size="xs"
        :data-pack-install="row.language"
        :disabled="isBusy(row)"
        @click="emit('install', row.language)"
      >{{ t('language_pack_install') }}</Button>
      <!-- Same route as Install (`ConfigView`'s `installLanguage`): the core
           does not distinguish a first install from a reinstall over a
           newer offer. -->
      <Button
        v-if="updateAvailable(row)"
        variant="outline" size="xs"
        :data-pack-update="row.language"
        :disabled="isBusy(row)"
        @click="emit('install', row.language)"
      >{{ t('language_pack_update') }}</Button>
      <!-- Only an installed pack can be removed. -->
      <Button
        v-if="row.installed !== null"
        variant="outline" size="xs"
        :data-pack-remove="row.language"
        :disabled="isBusy(row)"
        @click="emit('remove', row.language)"
      >{{ t('language_pack_remove') }}</Button>
    </div>
  </div>
</template>
