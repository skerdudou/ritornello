<script setup lang="ts">
import {
  api, Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { refusedNote } from '../composables/refusedNote'
import { useCatalog } from '../composables/useCatalog'
import { checkFailure, hasUsableCheck, useUpdateCheck } from '../composables/useUpdateCheck'
import type { ComponentOffer, UpdatePayload } from '../types'
import UpdateCheckStatus from './UpdateCheckStatus.vue'

const props = defineProps<{
  open: boolean
  components: ComponentOffer[]
  outcome: UpdatePayload['outcome']
  /** `UpdatePayload.last_check_unix_s` — see `hasUsableCheck` below. */
  lastCheckUnixS: number | null
  /** `UpdatePayload.busy` — a job is running, so Install must not re-enqueue
   *  a second one on the same component (m6, the update card's own Install
   *  already does this). */
  busy: string | null
}>()
const emit = defineEmits<{
  'update:open': [boolean]
  install: [string]
  /** A plugin a third-party source offers fresh: the page asks the second
   *  consent (spec §4.5), naming the repository, before anything is sent. */
  'install-third-party': [name: string, repo: string]
  refresh: []
}>()
const { t } = useCatalog()

/**
 * Opening this dialog looks for components by itself: an empty list on a
 * device nobody had checked yet used to read as "there is nothing", and the
 * operator had to know to go and press Check first. The page reloads its
 * update state on `refresh`, which is what fills `components`.
 */
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

type CatalogueEntry = { kinds: string[]; description: string }

/** What the release says about its components, `null` until asked. */
const catalogue = ref<Record<string, CatalogueEntry> | null>(null)
const asked = ref(false)

/**
 * What each third-party source says about the plugins it offers fresh, by
 * lowercased `owner/repo` (`GET /api/update/catalogue?repo=`). The core only
 * ever returns the entries for names that source is offered, and the rows
 * below only ever read a source's entries for a row of that same source:
 * neither side alone lets a stranger describe a name it does not offer.
 */
const sourceCatalogues = ref<Record<string, Record<string, CatalogueEntry>>>({})
/** Repositories asked, or being asked: one request per source per page. */
const sourcesAsked = new Set<string>()

/**
 * Components the release publishes and this device does not have.
 *
 * **Never a language pack** (arbitration C19, written after task 7's
 * review): a pack does not travel through `POST /api/update/install` at
 * all — `carries()` answers `false` for `Offer::LanguagePack` by design, a
 * pack never rides the components' install path — so a row here whose
 * "Install" called `installPlugin()` produced "nothing published for
 * ritornello-lang-fr" even though the very same row showed an offered
 * version. `AddLanguageDialog.vue` (opened from `ConfigView`'s "Language and
 * display" card) is the one place a pack is first installed from, because
 * it is the only one that knows the right route (`POST
 * /api/languages/{language}`). This was once going to be a second category
 * inside this same dialog; it never happened, and it will not: the two
 * gestures need two different routes, so they stay two different surfaces
 * — this one is "Add a plugin", that one its mirror.
 */
const rows = computed(() =>
  props.components
    .filter((c) => c.availability === 'not_installed' && c.kind !== 'language_pack')
    .map((c) => ({ offer: c, entry: entryFor(c) })),
)

/**
 * A row's description, from the catalogue of **where it comes from**: ours
 * for our rows, its own source's for a third-party one — never ours for a
 * stranger's name, nor a stranger's for ours. A contested row has no single
 * source, so nothing describes it.
 */
function entryFor(c: ComponentOffer): CatalogueEntry | null {
  if (c.kind !== 'third_party') return catalogue.value?.[c.name] ?? null
  if (c.conflict_repos || !c.third_party_repo) return null
  return sourceCatalogues.value[c.third_party_repo.toLowerCase()]?.[c.name] ?? null
}

/** A third-party row offered by exactly one source: installable, after the
 *  second consent. */
function isFreshThirdParty(c: ComponentOffer): c is ComponentOffer & { third_party_repo: string } {
  return c.kind === 'third_party' && !c.conflict_repos && !!c.third_party_repo
}

/** Install on a row: ours at once, a stranger's through the page's
 *  confirmation naming its repository (spec §4.5). */
function onInstall(c: ComponentOffer) {
  // Fails closed: a third-party row that is not a fresh offer from one named
  // source never installs directly, whatever shape it arrives in.
  if (c.kind !== 'third_party') emit('install', c.name)
  else if (isFreshThirdParty(c)) emit('install-third-party', c.name, c.third_party_repo)
}

/** Whether a row has anything to install from here: ours, or a stranger's
 *  fresh offer from one named source. A third-party row with no source to
 *  name gets no button rather than a direct install. */
function canInstall(c: ComponentOffer): boolean {
  return c.kind !== 'third_party' || isFreshThirdParty(c)
}

/** The sources whose fresh offers are on screen. */
const freshRepos = computed(() => [
  ...new Set(props.components.filter(isFreshThirdParty).map((c) => c.third_party_repo.toLowerCase())),
])

watch(
  () => [props.open, freshRepos.value] as const,
  ([open, repos]) => {
    if (!open) return
    for (const repo of repos) {
      if (sourcesAsked.has(repo)) continue
      sourcesAsked.add(repo)
      api
        .get<{ components: Record<string, CatalogueEntry> }>(
          `/api/update/catalogue?repo=${encodeURIComponent(repo)}`,
        )
        .then((answer) => {
          sourceCatalogues.value = { ...sourceCatalogues.value, [repo]: answer.components }
        })
        .catch((e) => {
          // No description is the documented fallback (name, source,
          // version); a later opening asks again rather than latching a
          // failure as "this source publishes nothing".
          console.warn('source catalogue unavailable', repo, e)
          sourcesAsked.delete(repo)
        })
    }
  },
  { immediate: true },
)

/**
 * The release published no catalogue at all — distinguished from "this
 * device has everything", which is `rows.length === 0`.
 *
 * The original form of this predicate also carried `rows.value.length > 0`
 * and `asked.value` as conjuncts. Both are dropped here, having been proven
 * structurally redundant rather than merely convenient — proof, not
 * assumption, since this plan has repeatedly hit predicates credited with
 * coverage a fixture could never actually exercise:
 * - `rows.value.length > 0`: this computed is read from exactly one
 *   template site, nested in `<template v-else>` — the sibling of
 *   `v-if="rows.length === 0"` — so `rows.length > 0` already holds by
 *   construction at the only place this value is ever read.
 * - `asked.value`: read from that same single site, which is only in the
 *   DOM at all while `open` is true — `DialogContent` does not render its
 *   slot while closed (confirmed by mounting closed: an empty teleport and
 *   zero calls to `/api/update/catalogue`). The `watch` below sets
 *   `asked.value = true` synchronously, before its first `await`, in the
 *   same pre-render flush Vue runs watchers in — so by the time any render
 *   showing this dialog open can happen, `asked` already holds. `asked`
 *   itself is not removed: it still guards the `watch`'s own re-fetch below,
 *   an unrelated role from the one it played here.
 */
const noCatalogue = computed(
  // Our catalogue only speaks for our rows: a list made of strangers' offers
  // alone must not blame our release for not describing them.
  () => rows.value.some((r) => r.offer.kind !== 'third_party')
    && Object.keys(catalogue.value ?? {}).length === 0,
)

/** See `hasUsableCheck`: whether an empty `rows` means "nothing to add". */
const usableCheck = computed(() => hasUsableCheck(props.outcome, props.lastCheckUnixS))

/**
 * The check ran and found no release of ours for this appliance — nothing
 * published, or only betas it declines. A state, not a failure, and the one
 * every stable-channel device is in today: the core then offers no stranger's
 * plugin either (ownership cannot be judged against a release it did not
 * read), so "no usable check has run" would be false and "nothing to add"
 * would hide why.
 */
const noRelease = computed(
  () => props.outcome.kind === 'no_release' || props.outcome.kind === 'only_prereleases',
)


// Generation counter, as `PluginRoute.vue` does for a plugin catalogue: the
// request is asynchronous, and a dialog closed and reopened must not have a
// late answer land under it.
let generation = 0

watch(
  () => props.open,
  async (open) => {
    // `immediate` is not a stylistic default here and the reason is
    // measured: `Dialog` stays mounted when closed (see
    // `UpdateDialog.vue`'s own note), and the page mounts this with `open`
    // already true in a test — a plain watch never fires for the value a
    // prop already held at mount.
    if (!open || asked.value) return
    const localGeneration = ++generation
    // Asked once per page, not once per opening: the core keeps it for the
    // life of its session, keyed by the tag-qualified URL, and re-asking
    // would defeat that from this side.
    asked.value = true
    try {
      const answer = await api.get<{
        components: Record<string, { kinds: string[]; description: string }>
      }>('/api/update/catalogue')
      if (localGeneration === generation) catalogue.value = answer.components
    } catch (e) {
      // An unreachable catalogue must not close the dialog: names alone stay
      // useful, and installing does not depend on a description. Nor must
      // this attempt latch as final: `asked` goes back to `false` so the next
      // opening retries instead of repeating this one failure — a request
      // this page itself could not complete (the core unreachable, a route
      // error) is not the same fact as "this release has no catalogue", and
      // must not be remembered as long as the fetch that answered it was.
      console.warn('update catalogue unavailable', e)
      asked.value = false
      if (localGeneration === generation) catalogue.value = {}
    }
  },
  { immediate: true },
)
</script>

<template>
  <Dialog :open="open" @update:open="(v: boolean) => emit('update:open', v)">
    <DialogContent data-installables-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('installables_title') }}</DialogTitle>
        <DialogDescription>{{ t('installables_description') }}</DialogDescription>
      </DialogHeader>

      <UpdateCheckStatus :phase="check.phase.value" :failure="failure" @retry="check.retry()" />

      <!-- While the check runs the list is not shown at all, not even as
           "nothing to add": it would be an answer nobody has given yet. -->
      <template v-if="check.phase.value === 'checking'" />
      <p
        v-else-if="rows.length === 0 && noRelease"
        data-installables-no-release
        class="text-sm text-muted-foreground"
      >
        {{ t('installables_no_release') }}
      </p>
      <p
        v-else-if="rows.length === 0 && !usableCheck"
        data-installables-unknown
        class="text-sm text-muted-foreground"
      >
        {{ t('installables_unknown') }}
      </p>
      <p v-else-if="rows.length === 0" data-installables-empty class="text-sm text-muted-foreground">
        {{ t('installables_empty') }}
      </p>
      <template v-else>
        <p v-if="noCatalogue" data-installables-no-catalogue class="text-sm text-muted-foreground">
          {{ t('installables_no_catalogue') }}
        </p>
        <ul class="space-y-3">
          <li
            v-for="row in rows"
            :key="row.offer.name"
            data-installable-row
            :data-name="row.offer.name"
            class="flex items-start justify-between gap-2"
          >
            <div class="grid gap-0.5 text-sm">
              <span>{{ row.offer.name }}</span>
              <!-- Where a stranger's offer comes from, and which version:
                   with no catalogue of its own, that is all there is to say
                   (spec §6: name, source, version). -->
              <span
                v-if="isFreshThirdParty(row.offer)"
                data-installable-repo
                class="text-xs text-muted-foreground"
              >{{ t('installables_from_repo', { repo: row.offer.third_party_repo, version: row.offer.offered ?? '?' }) }}</span>
              <!-- A type is an IHM word, never the release's raw catalogue
                   string: it goes through the language catalogue like every
                   other label on this page (the French pack already has the
                   vocabulary to translate it). A template literal, not string
                   concatenation, so that `i18nKeysUsed.test.ts`'s literal-key
                   scanner — which only recognises a quoted string immediately
                   after `t(` — does not mistake the dynamic prefix
                   `plugin_kind_` for a key of its own. -->
              <span
                v-if="row.entry"
                data-installable-kind
              >{{ row.entry.kinds.map((k) => t(`plugin_kind_${k}`)).join(', ') }}</span>
              <span v-if="row.entry" data-installable-description class="text-xs text-muted-foreground">
                {{ row.entry.description }}
              </span>
            </div>
            <!-- `installable === false` names a privileged plugin (`files`
                 today): its packaging places a root service, a systemd unit
                 or a polkit rule this page cannot place, so an Install
                 button here could only ever fail. `ritornello-install` does
                 the whole job instead, in the same sentence
                 `ConfigView.vue`'s table shows for the same reason. -->
            <!-- Two or more sources offer this name: none is believed, so
                 there is nothing to install, and every one is named so the
                 operator can remove the one they did not mean to read. -->
            <span
              v-if="row.offer.conflict_repos"
              data-installable-conflict
              class="text-xs text-muted-foreground"
            >{{ t('installables_conflict', { repos: row.offer.conflict_repos.join(', ') }) }}</span>
            <!-- The sentence follows why the row is refused: a release that
                 publishes no contracts for it says so whatever the row is
                 (the same sentence the update dialog shows); otherwise it
                 follows what the row is (`refusedNote`): only one of ours is
                 "privileged"; a stranger's plugin was refused for what its
                 archive carried. -->
            <span
              v-else-if="row.offer.installable === false"
              data-installable-privileged
              class="text-xs text-muted-foreground"
            >{{ row.offer.not_installable_reason === 'contracts_unpublished'
              ? t('update_row_contracts_unpublished', { component: row.offer.name })
              : refusedNote(t, row.offer) }}</span>
            <!-- Never disabled for a missing description: installing does not
                 depend on knowing how to describe the component. Disabled
                 while `busy` (m6): the update card's own Install button
                 already does this, and without it a second press here
                 re-enqueues a second `Job::Install` of a component whose
                 first install has not finished yet. -->
            <Button
              v-else-if="canInstall(row.offer)"
              variant="outline" size="xs" data-installable-install
              :disabled="!!busy"
              @click="onInstall(row.offer)"
            >{{ t('installables_install') }}</Button>
          </li>
        </ul>
      </template>
    </DialogContent>
  </Dialog>
</template>
