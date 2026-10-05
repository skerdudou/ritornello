<script setup lang="ts">
/**
 * One line per language a pack is installed for, sitting right under
 * `LanguageCard`'s selector: what is on the device, with Update and Remove.
 * Adding a language that is only offered is `AddLanguageDialog`'s job — one
 * gesture per language, everything published for it at once, never plugin by
 * plugin.
 *
 * A pure render component, exactly like `LanguageCard`: props in, events
 * out, **no API call here**. `ConfigView.vue` owns `POST`/`DELETE
 * /api/languages/{language}` and the confirmation dialog in front of
 * "Remove" — this component only ever emits the language code.
 *
 * **The card's arbitrated rule**: nothing is shown where there is no news.
 * A language with no pack installed renders no line at
 * all — the same convention `LanguageCard`'s own `annotation()` uses for a
 * complete language.
 */
import { Button } from '@ritornello/ui'
import { computed } from 'vue'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@ritornello/ui'
import { packSourceLabel } from '../composables/packSource'
import type {
  LanguageBusy, LanguagePackDetail, LanguagePackOverlap, LanguagePackRow as PackRow, LocalePayload,
} from '../types'

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
  /**
   * What the core said when it refused the last preference write (422: the
   * pack does not carry the module, the ceiling; 503: core busy), with the
   * language and module it was for — shown next to that module's select.
   */
  preferenceError?: { language: string; module: string; message: string } | null
}>()
const emit = defineEmits<{
  install: [string]
  remove: [string]
  /** `ConfigView` writes the preference and reloads `/api/locale`. */
  prefer: [language: string, module: string, pack: string]
}>()

const { t } = useCatalog()

/**
 * The rows worth a line: the languages with a pack on disk — **any** pack,
 * not only the official one: a language whose only installed pack is a third
 * party's must keep its Update and Remove. A language that is only *offered*
 * is `AddLanguageDialog`'s row, not this one — listing it here as well would
 * put the same gesture in two places, and this list would grow with every
 * language a release publishes whether or not the owner wants it.
 */
const rows = computed<PackRow[]>(() =>
  props.payload.packs.filter((p) => p.packs.some((x) => x.installed !== null)),
)

/** `row.update_available`: decided by the core over **every** pack of the
 * language, never re-derived here from the official pack's two versions. */
function updateAvailable(row: PackRow): boolean {
  return row.update_available
}

function sourceLabel(pack: LanguagePackDetail): string {
  return packSourceLabel(t.value, pack.source)
}

/** The label of the pack `id` within `row`. Two packs from one repository
 * (it can publish several) would read the same, so the id is appended then. */
function packLabel(row: PackRow, id: string): string {
  const pack = row.packs.find((p) => p.id === id)
  if (!pack) return id
  const label = sourceLabel(pack)
  return row.packs.filter((p) => sourceLabel(p) === label).length > 1 ? `${label} (${id})` : label
}

/** Whether a preference error belongs to this select. */
function errorFor(row: PackRow, overlap: LanguagePackOverlap): string | null {
  const e = props.preferenceError
  return e && e.language === row.language && e.module === overlap.module ? e.message : null
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
      <!-- Under a language with several packs: where each one comes from.
           A single-pack language renders exactly as it always did. -->
      <ul v-if="row.packs.length > 1" class="w-full list-none space-y-1 text-xs text-muted-foreground" data-pack-sources>
        <li v-for="pack in row.packs" :key="pack.id" :data-pack-source="pack.id">
          {{ sourceLabel(pack) }} —
          {{ pack.installed ?? t('language_pack_not_installed') }}
        </li>
      </ul>
      <!-- Shown only where two installed packs carry the same module: which
           one speaks, and a choice. Nothing appears when no module is shared. -->
      <div v-if="row.overlaps.length > 0" class="w-full space-y-2" data-pack-overlaps>
        <p class="text-xs text-muted-foreground">{{ t('language_pack_overlap_intro') }}</p>
        <div
          v-for="overlap in row.overlaps"
          :key="overlap.module"
          class="flex flex-wrap items-center gap-2"
          :data-pack-overlap="overlap.module"
        >
          <span class="text-xs">{{ t('language_pack_overlap_module', { module: overlap.module }) }}</span>
          <Select
            :model-value="overlap.active"
            @update:model-value="(v) => emit('prefer', row.language, overlap.module, String(v))"
          >
            <!-- The label is rendered here, not left to reka's text capture
                 taken once at mount (the select-label pitfall). -->
            <SelectTrigger
              class="min-w-32"
              :data-pack-preference="overlap.module"
              :aria-label="t('language_pack_overlap_choose', { module: overlap.module })"
            ><SelectValue>{{ packLabel(row, overlap.active) }}</SelectValue></SelectTrigger>
            <SelectContent>
              <SelectItem v-for="id in overlap.packs" :key="id" :value="id">
                {{ packLabel(row, id) }}
              </SelectItem>
            </SelectContent>
          </Select>
          <span
            v-if="errorFor(row, overlap)"
            class="text-xs text-destructive"
            role="alert"
            data-pack-preference-error
          >{{ errorFor(row, overlap) }}</span>
        </div>
      </div>
      <!-- Same route as a first install (`ConfigView`'s `installLanguage`): the core
           does not distinguish a first install from a reinstall over a
           newer offer. -->
      <Button
        v-if="updateAvailable(row)"
        variant="outline" size="xs"
        :data-pack-update="row.language"
        :disabled="isBusy(row)"
        @click="emit('install', row.language)"
      >{{ t('language_pack_update') }}</Button>
      <!-- Every row here has a pack on disk, so every row can be removed. -->
      <Button
        variant="outline" size="xs"
        :data-pack-remove="row.language"
        :disabled="isBusy(row)"
        @click="emit('remove', row.language)"
      >{{ t('language_pack_remove') }}</Button>
    </div>
  </div>
</template>
