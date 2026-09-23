<script setup lang="ts">
import {
  api, Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { ComponentOffer, UpdatePayload } from '../types'

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
const emit = defineEmits<{ 'update:open': [boolean]; install: [string] }>()
const { t } = useCatalog()

/** What the release says about its components, `null` until asked. */
const catalogue = ref<Record<string, { kinds: string[]; description: string }> | null>(null)
const asked = ref(false)

/**
 * Components the release publishes and this device does not have.
 *
 * **Never a language pack** (arbitration C19, written after task 7's
 * review): a pack does not travel through `POST /api/update/install` at
 * all — `carries()` answers `false` for `Offer::LanguagePack` by design, a
 * pack never rides the components' install path — so a row here whose
 * "Install" called `installPlugin()` produced "nothing published for
 * ritornello-lang-fr" even though the very same row showed an offered
 * version. `LanguagePacksRow.vue` (`ConfigView`'s "Language and display"
 * card) is the one place a pack installs from, because it is the only one
 * that knows the right route (`POST /api/languages/{language}`). This was
 * once going to be a second category inside this same dialog; it never
 * happened, and it will not: the two gestures need two different routes,
 * so they stay two different surfaces.
 */
const rows = computed(() =>
  props.components
    .filter((c) => c.availability === 'not_installed' && c.kind !== 'language_pack')
    .map((c) => ({ offer: c, entry: catalogue.value?.[c.name] ?? null })),
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
const noCatalogue = computed(() => Object.keys(catalogue.value ?? {}).length === 0)

/**
 * Whether the last check actually looked, and can therefore be trusted to
 * mean "nothing to add" when `rows` comes back empty.
 *
 * `never_checked`, `no_release` and `only_prereleases` always rebuild every
 * component against an empty published list server-side (`component_offers`
 * called with `&[]`, see `update/mod.rs`'s `check`), so every row resolves to
 * `unknown` and `rows` is empty regardless of what the appliance actually has
 * — an empty `rows` there is silence, not completeness.
 *
 * `failed` is not always that kind of silence (N3). It is published both for
 * a failed check (`publish_failure`, which leaves `components` exactly as an
 * earlier successful check left them) and for a *refused install* that
 * followed a successful check (`install_report`/`conclude_install`, which
 * re-checks before installing) — in both cases the rows on screen, and this
 * `lastCheckUnixS`, are still the real ones from that earlier success. Only a
 * `failed` on a device that has genuinely **never** succeeded — `null` here —
 * is the silent kind. `ok` and `installed` (the transient report right after
 * a successful install, still built from a real release) are never silent
 * either way.
 */
const hasUsableCheck = computed(
  () =>
    props.outcome.kind === 'ok'
    || props.outcome.kind === 'installed'
    || (props.outcome.kind === 'failed' && props.lastCheckUnixS !== null),
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

      <p
        v-if="rows.length === 0 && !hasUsableCheck"
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
            <span
              v-if="row.offer.installable === false"
              data-installable-privileged
              class="text-xs text-muted-foreground"
            >{{ t('plugin_privileged_note') }}</span>
            <!-- Never disabled for a missing description: installing does not
                 depend on knowing how to describe the component. Disabled
                 while `busy` (m6): the update card's own Install button
                 already does this, and without it a second press here
                 re-enqueues a second `Job::Install` of a component whose
                 first install has not finished yet. -->
            <Button
              v-else
              variant="outline" size="xs" data-installable-install
              :disabled="!!busy"
              @click="emit('install', row.offer.name)"
            >{{ t('installables_install') }}</Button>
          </li>
        </ul>
      </template>
    </DialogContent>
  </Dialog>
</template>
