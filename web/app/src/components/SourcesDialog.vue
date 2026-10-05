<script setup lang="ts">
import {
  api, Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, Input,
} from '@ritornello/ui'
import { ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { SourceReport, SourceRow } from '../types'

/**
 * The repositories this device reads when it looks for updates
 * (`GET /api/update/sources`), and the two gestures the operator has on that
 * list: add one, remove one they added.
 *
 * The dialog fetches by itself when it opens, with the generation-counter
 * pattern of `InstallablesDialog.vue`: a request that comes back after the
 * dialog was closed and reopened must not land under the newer one.
 *
 * **What a row may do** (the owner's arbitrations): the official repository
 * is listed first and is read-only; a repository a plugin announced is shown
 * read-only too, and carries a remove button only when the operator has also
 * stored it, since removing the stored copy is all the button can do — the
 * announcement stays. An added row is removable.
 */
const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ 'update:open': [boolean] }>()
const { t } = useCatalog()

const rows = ref<SourceRow[] | null>(null)
const loadFailed = ref(false)
const draft = ref('')
/** The message of the last refused write, shown inline; `null` when none. */
const failure = ref<string | null>(null)
const working = ref(false)

let generation = 0

async function load() {
  const mine = ++generation
  try {
    const answer = await api.get<SourceRow[]>('/api/update/sources')
    if (mine !== generation) return
    rows.value = answer
    loadFailed.value = false
  } catch (e) {
    console.warn('update sources unavailable', e)
    if (mine === generation) loadFailed.value = true
  }
}

watch(
  () => props.open,
  (open) => {
    if (!open) return
    failure.value = null
    void load()
  },
  { immediate: true },
)

/**
 * A write, answered with what the operator may read. A refusal the core
 * explained carries its catalogue message as `error` and that is shown. Two
 * cases carry nothing an operator can act on and get one generic sentence
 * instead: a full write channel (429 or 500, **no body**) and a request that
 * never reached the core (the browser's raw `fetch` text, in whatever
 * language the browser speaks). The kit's `api.post` folds all three into one
 * string, so this reads the response itself to tell them apart.
 */
async function write(method: 'POST' | 'DELETE', url: string, body?: unknown): Promise<string | null> {
  let response: Response
  try {
    response = await fetch(url, {
      method,
      headers: { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  } catch {
    return t.value('update_source_failed')
  }
  if (response.ok) return null
  try {
    const parsed = (await response.json()) as { error?: unknown }
    if (typeof parsed.error === 'string' && parsed.error !== '') return parsed.error
  } catch {
    // no body, or not JSON: the generic sentence below
  }
  return t.value('update_source_failed')
}

/**
 * Whether the operator can take this row out of the list: only a repository
 * the operator stored, and never the official one, whatever it claims.
 */
function removable(row: SourceRow): boolean {
  return row.stored && row.kind !== 'official'
}

async function add() {
  const repo = draft.value.trim()
  if (repo === '' || working.value) return
  working.value = true
  failure.value = null
  try {
    const error = await write('POST', '/api/update/sources', { repo })
    if (error !== null) {
      failure.value = error
      return
    }
    draft.value = ''
    await load()
  } finally {
    working.value = false
  }
}

async function remove(row: SourceRow) {
  if (working.value) return
  working.value = true
  failure.value = null
  try {
    const path = row.repo.split('/').map(encodeURIComponent).join('/')
    const error = await write('DELETE', `/api/update/sources/${path}`)
    if (error !== null) {
      failure.value = error
      return
    }
    await load()
  } finally {
    working.value = false
  }
}

/**
 * What the last check learned from a repository, in words. `null` and
 * "answered, published nothing" are two different sentences, and neither is
 * ever "up to date": a source that was not asked, or did not answer, has said
 * nothing about being current.
 */
function reportLines(report: SourceReport | null): string[] {
  if (report === null) return [t.value('update_source_not_checked')]
  if (!report.answered) return [t.value('update_source_unanswered')]
  const parts: string[] = []
  if (report.plugins.length > 0) {
    parts.push(t.value('update_source_published_plugins', { plugins: report.plugins.join(', ') }))
  }
  if (report.languages.length > 0) {
    parts.push(t.value('update_source_published_languages', { languages: report.languages.join(', ') }))
  }
  return parts.length > 0 ? parts : [t.value('update_source_publishes_nothing')]
}
</script>

<template>
  <Dialog :open="open" @update:open="(v: boolean) => emit('update:open', v)">
    <DialogContent data-sources-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('update_sources_title') }}</DialogTitle>
        <DialogDescription>{{ t('update_sources_description') }}</DialogDescription>
      </DialogHeader>

      <p v-if="loadFailed && rows === null" data-sources-unreadable class="text-sm text-destructive">
        {{ t('update_sources_unreadable') }}
      </p>

      <ul v-if="rows" class="space-y-3">
        <li
          v-for="row in rows"
          :key="row.repo"
          data-source-row
          :data-repo="row.repo"
          :data-kind="row.kind"
          class="flex items-start justify-between gap-2"
        >
          <div class="grid min-w-0 gap-0.5 text-sm">
            <span class="break-all font-medium">{{ row.repo }}</span>
            <span v-if="row.kind === 'official'" data-source-official class="text-xs text-muted-foreground">
              {{ t('update_source_official') }}
            </span>
            <span v-else-if="row.kind === 'announced'" data-source-announced class="text-xs text-muted-foreground">
              {{ t('update_source_announced_by', { plugins: row.announced_by.join(', ') }) }}
            </span>
            <span v-if="!row.queryable" data-source-not-queryable class="text-xs text-muted-foreground">
              {{ t('update_source_not_queryable') }}
            </span>
            <span
              v-for="line in reportLines(row.report)"
              :key="line"
              data-source-report
              class="text-xs text-muted-foreground"
            >{{ line }}</span>
          </div>
          <Button
            v-if="removable(row)"
            variant="outline" size="xs" data-source-remove
            :disabled="working"
            @click="remove(row)"
          >{{ t('update_source_remove') }}</Button>
        </li>
      </ul>

      <form class="flex items-end gap-2" data-source-add-form @submit.prevent="add">
        <label class="grid flex-1 gap-1 text-sm">
          {{ t('update_source_add_label') }}
          <Input v-model="draft" data-source-input autocomplete="off" spellcheck="false" />
        </label>
        <Button type="submit" data-source-add :disabled="working || draft.trim() === ''">
          {{ t('update_source_add') }}
        </Button>
      </form>

      <p v-if="failure" data-source-error class="text-sm text-destructive">{{ failure }}</p>
    </DialogContent>
  </Dialog>
</template>
