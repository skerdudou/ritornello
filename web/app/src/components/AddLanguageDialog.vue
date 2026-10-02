<script setup lang="ts">
/**
 * "Add a language": the mirror of `InstallablesDialog.vue` for the language
 * packs the release offers and this device does not have. Opened from the
 * language card, because choosing to ADD a language is a different question
 * from managing the ones already installed (`LanguagePacksRow.vue`, which now
 * lists installed packs only, with Update and Remove).
 *
 * Like `LanguagePacksRow`, no install logic lives here: a click emits the
 * language code and `ConfigView`'s `installLanguage` posts `POST
 * /api/languages/{language}` — the one route a pack travels through, never
 * `POST /api/update/install`.
 *
 * Like `InstallablesDialog`, opening it runs the update check by itself
 * (`useUpdateCheck`), so an empty list never means "nobody has looked".
 */
import { Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@ritornello/ui'
import { computed } from 'vue'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import { checkFailure, hasUsableCheck, useUpdateCheck } from '../composables/useUpdateCheck'
import type { LanguageBusy, LanguagePackRow as PackRow, UpdatePayload } from '../types'
import UpdateCheckStatus from './UpdateCheckStatus.vue'

const props = defineProps<{
  open: boolean
  /** `LocalePayload.packs`. */
  packs: PackRow[]
  outcome: UpdatePayload['outcome']
  lastCheckUnixS: number | null
  /** `UpdatePayload.busy`: a job is running, which the check only waits for. */
  busy: string | null
  /** The language gesture `ConfigView` has enqueued, or `null`. */
  packBusy: LanguageBusy | null
}>()
const emit = defineEmits<{ 'update:open': [boolean]; install: [string]; refresh: [] }>()
const { t } = useCatalog()

const check = useUpdateCheck({
  open: () => props.open,
  state: () => ({
    outcome: props.outcome,
    lastCheckUnixS: props.lastCheckUnixS,
    busy: props.busy,
  }),
  onSettled: () => emit('refresh'),
})
const failure = computed(() => checkFailure(check.phase.value, check.error.value, props.outcome))

/** Offered by the release and not on disk: the only rows this dialog is for. */
const rows = computed<PackRow[]>(() =>
  props.packs.filter((p) => p.offered !== null && p.installed === null),
)
const usableCheck = computed(() => hasUsableCheck(props.outcome, props.lastCheckUnixS))

function isBusy(row: PackRow): boolean {
  return props.packBusy !== null && props.packBusy.language === row.language
}
</script>

<template>
  <Dialog :open="open" @update:open="(v: boolean) => emit('update:open', v)">
    <DialogContent data-add-language-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('languages_add_title') }}</DialogTitle>
        <DialogDescription>{{ t('languages_add_description') }}</DialogDescription>
      </DialogHeader>

      <UpdateCheckStatus :phase="check.phase.value" :failure="failure" @retry="check.retry()" />

      <template v-if="check.phase.value === 'checking'" />
      <p
        v-else-if="rows.length === 0 && !usableCheck"
        data-add-language-unknown
        class="text-sm text-muted-foreground"
      >
        {{ t('installables_unknown') }}
      </p>
      <p v-else-if="rows.length === 0" data-add-language-empty class="text-sm text-muted-foreground">
        {{ t('languages_add_empty') }}
      </p>
      <ul v-else class="space-y-3">
        <li
          v-for="row in rows"
          :key="row.language"
          data-add-language-row
          :data-language="row.language"
          class="flex flex-wrap items-center justify-between gap-2 text-sm"
        >
          <span>{{ languageName(row.language) }}</span>
          <span v-if="isBusy(row)" class="text-xs text-muted-foreground" data-pack-busy>
            {{ t('language_pack_installing', { language: languageName(row.language) }) }}
          </span>
          <!-- Disabled while any language gesture is in flight, not only this
               row's: two installs racing would both be enqueued. -->
          <Button
            variant="outline" size="xs"
            :data-pack-install="row.language"
            :disabled="packBusy !== null"
            @click="emit('install', row.language)"
          >{{ t('language_pack_install') }}</Button>
        </li>
      </ul>
    </DialogContent>
  </Dialog>
</template>
