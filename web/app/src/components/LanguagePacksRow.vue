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
import { computed, ref } from 'vue'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import type { LanguagePackRow as PackRow, LocalePayload } from '../types'

const props = defineProps<{
  payload: LocalePayload
  /** Language whose install/remove request is in flight, or `null` — the
   *  same convention as `ConfigView`'s `inProgress`/`uninstallTarget`, but a
   *  single value: only one language gesture can be queued from this row at
   *  a time. */
  busy: string | null
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

/**
 * Which gesture this row's own click put in flight, tracked locally rather
 * than guessed from `row.installed`: a row with a pack already installed can
 * show both "Update" and "Remove" at once, and `busy` alone (just the
 * language code, the same convention `ConfigView` already uses elsewhere)
 * cannot tell the two apart once the request is in flight — both leave
 * `row.installed` non-null. Needs no reset on completion: the next gesture,
 * on any row, overwrites it before it emits, and this value is only ever
 * read while `busy` still names this row's own language.
 */
const pendingAction = ref<'install' | 'remove' | null>(null)

function onInstall(language: string) {
  pendingAction.value = 'install'
  emit('install', language)
}

function onRemove(language: string) {
  pendingAction.value = 'remove'
  emit('remove', language)
}

function busyLabel(row: PackRow): string {
  const language = languageName(row.language)
  return pendingAction.value === 'remove'
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
      <span v-if="busy === row.language" class="text-xs text-muted-foreground" data-pack-busy>
        {{ busyLabel(row) }}
      </span>
      <!-- Only a language with nothing on disk yet can be installed. -->
      <Button
        v-if="row.installed === null"
        variant="outline" size="xs"
        :data-pack-install="row.language"
        :disabled="busy === row.language"
        @click="onInstall(row.language)"
      >{{ t('language_pack_install') }}</Button>
      <!-- Same route as Install (`ConfigView`'s `installLanguage`): the core
           does not distinguish a first install from a reinstall over a
           newer offer. -->
      <Button
        v-if="updateAvailable(row)"
        variant="outline" size="xs"
        :data-pack-update="row.language"
        :disabled="busy === row.language"
        @click="onInstall(row.language)"
      >{{ t('language_pack_update') }}</Button>
      <!-- Only an installed pack can be removed. -->
      <Button
        v-if="row.installed !== null"
        variant="outline" size="xs"
        :data-pack-remove="row.language"
        :disabled="busy === row.language"
        @click="onRemove(row.language)"
      >{{ t('language_pack_remove') }}</Button>
    </div>
  </div>
</template>
