<script setup lang="ts">
import { Button, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { computed } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { UpdatePayload } from '../types'

/**
 * The device's own view of itself against the last release it read: one
 * line, an error or a rollback note when there is one, and the two gestures
 * — check, install.
 *
 * **No network call of its own.** It takes its payload as a prop and emits
 * `check`/`install`; the page that mounts it owns `GET /api/update` and the
 * two `POST`s. This is what makes the card testable without stubbing
 * `fetch` — the same shape as `CoverCacheDetails.vue`'s panel, one level up
 * (that one reads its own snapshot; this one is handed one because two
 * different buttons on the same page — Check and Install — must react to the
 * very same payload without racing each other's reload).
 */
const props = defineProps<{ update: UpdatePayload }>()
const emit = defineEmits<{ check: []; install: [] }>()
const { t } = useCatalog()

/** Components an install would act on: everything out of step, core included. */
const actionable = computed(() =>
  props.update.components.filter((c) => c.availability === 'update_available'),
)

const summary = computed(() => {
  const { outcome, release_version } = props.update
  if (outcome.kind === 'no_release') return t.value('update_no_release')
  if (outcome.kind === 'never_checked') return t.value('update_never_checked')
  const core = props.update.components.find((c) => c.kind === 'core')
  if (!core) return t.value('update_unknown')
  // Both numbers, in order, so a swap is visible. The arrow is punctuation
  // and not a word, so it needs no catalog entry.
  if (core.availability === 'update_available' && core.installed && release_version)
    return `${core.installed} → ${release_version}`
  return t.value('update_aligned')
})

/**
 * Read through a `computed` rather than in the template: narrowing a
 * discriminated union across a template `v-if` is not something `vue-tsc`
 * can be trusted to carry into a sibling expression, where this file's own
 * `outcome.kind === 'failed'` check would otherwise need `outcome.detail`
 * read a second time.
 */
const errorDetail = computed(() =>
  props.update.outcome.kind === 'failed' ? props.update.outcome.detail : null,
)

/**
 * The transient report of a just-finished install (`CheckOutcome::Installed`,
 * Ruling 55). Shown alongside `summary`, never instead of it: the row itself
 * flipping to "up to date" is the durable signal, and this is only the
 * one-off sentence naming what just happened.
 */
const installedDetail = computed(() =>
  props.update.outcome.kind === 'installed' ? props.update.outcome.detail : null,
)

/**
 * What the core's own last-installed archive did not touch — the privileged
 * installer, the systemd units, the polkit rules `install_one` never places.
 * `undefined` until a core install has actually happened, and never an empty
 * array once it has: the core's archive always carries something here (see
 * `archive::core_not_installed`'s own doc comment).
 */
const coreArchiveNoteCount = computed(() => {
  const core = props.update.components.find((c) => c.kind === 'core')
  return core?.not_installed_files?.length ?? 0
})
</script>

<template>
  <Card data-update-card>
    <CardHeader><CardTitle>{{ t('update_title') }}</CardTitle></CardHeader>
    <CardContent class="space-y-2">
      <p data-update-summary class="text-sm font-medium">{{ summary }}</p>

      <p v-if="errorDetail" data-update-error class="text-sm text-destructive">{{ errorDetail }}</p>
      <!-- A separate line from the error itself: `data-update-error`'s text
           is asserted verbatim by `toBe` in the test suite, so the plain
           statement of this chantier's decision 50-2 (only the first cause of
           a multi-component gesture reaches this payload; every cause is in
           the log) cannot share that element without breaking that test. -->
      <p v-if="errorDetail" data-update-error-note class="text-xs text-muted-foreground">
        {{ t('update_partial_failure_note') }}
      </p>

      <p v-if="installedDetail" data-update-installed class="text-sm text-muted-foreground">
        {{ installedDetail }}
      </p>

      <!-- Without this, the only trace of a 3 a.m. rollback is a version
           number that did not move. -->
      <p v-if="update.last_rollback" data-update-rollback class="text-sm text-muted-foreground">
        {{ t('update_rolled_back') }}
      </p>

      <p v-if="coreArchiveNoteCount > 0" data-update-core-notes class="text-xs text-muted-foreground">
        {{ t('update_archive_notes', { count: coreArchiveNoteCount }) }}
      </p>

      <p v-if="update.busy" data-update-busy class="text-sm text-muted-foreground">{{ update.busy }}</p>

      <a
        v-if="update.release_url"
        :href="update.release_url"
        target="_blank"
        rel="noopener"
        data-update-release-link
        class="block text-xs text-muted-foreground underline"
      >
        {{ t('update_release_notes') }}
      </a>

      <div class="flex gap-2 pt-2">
        <Button
          variant="secondary"
          data-update-check
          :disabled="!!update.busy"
          @click="emit('check')"
        >
          {{ t('update_check') }}
        </Button>
        <Button
          data-update-install
          :disabled="!!update.busy || actionable.length === 0"
          @click="emit('install')"
        >
          {{ t('update_install') }}
        </Button>
      </div>
    </CardContent>
  </Card>
</template>
