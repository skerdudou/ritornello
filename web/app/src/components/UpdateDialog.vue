<script setup lang="ts">
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Switch,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { languageName } from '../composables/languages'
import { packLanguage } from '../composables/packSource'
import { refusedNote } from '../composables/refusedNote'
import { useCatalog } from '../composables/useCatalog'
import type { ComponentOffer } from '../types'

/**
 * The recap the operator sees before an install actually starts: one row per
 * component, pre-checked where the chantier's own policy says to pre-check,
 * and nowhere else — choosing to install something the device does not have,
 * or to touch a plugin whose own repository decides its version, stays the
 * operator's decision.
 *
 * Confirming emits the list of checked names; the actual `POST
 * /api/update/install` is the page's job, same division as `UpdateCard.vue`.
 */
const props = defineProps<{ open: boolean; components: ComponentOffer[] }>()
const emit = defineEmits<{ 'update:open': [boolean]; confirm: [string[]] }>()
const { t } = useCatalog()

const checked = ref<Set<string>>(new Set())

/**
 * The rows this dialog is about: components the device **has** and that are
 * not where the release says they should be.
 *
 * `not_installed` is deliberately excluded, and it is the whole point of this
 * change: choosing to add something the device does not have is a different
 * question, asked by `InstallablesDialog.vue`. Mixing the two is what made
 * this screen unreadable.
 *
 * `binary_missing` stays: installing is the repair for that row, and it is a
 * component `plugins.toml` declares. `undeclared` and `unknown` have nothing
 * to install here — the first is a stray binary, the second is a component
 * this release says nothing about.
 */
const relevant = computed(() =>
  props.components.filter(
    (c) => c.availability === 'update_available' || c.availability === 'binary_missing',
  ),
)

/**
 * What is out of step and nothing else: `update_available`, never
 * third-party (its own repository decides, not this release), and never a
 * component already known to need a manual step (checking it again would
 * only repeat the same refusal).
 *
 * "Third-party" is read from `third_party_repo` (ruling P6), not from the
 * kind alone: a stranger's **language pack** has the kind `language_pack`
 * and carries its source there, and used to be pre-ticked like one of ours.
 * The kind is kept as well, so a third-party plugin row that names no
 * repository fails closed too.
 */
function defaultChecked(components: ComponentOffer[]): Set<string> {
  return new Set(
    components
      .filter((c) => c.kind !== 'third_party' && !c.third_party_repo)
      .filter((c) => c.availability === 'update_available')
      .filter((c) => c.installable !== false)
      .map((c) => c.name),
  )
}

// Recomputed on every opening rather than once at mount: `Dialog` stays
// mounted when closed (see `CoverCacheDetails.vue`), and the components
// prop can have moved on since the last time this was open. `immediate`
// matters here and is not a stylistic default: the page mounts this dialog
// with `open` already `true` in a test (and, in principle, could do the same
// in production), and a plain `watch` never fires for the value a prop
// already held at mount — it only reacts to a *change*.
watch(
  () => props.open,
  (open) => {
    if (open) checked.value = defaultChecked(relevant.value)
  },
  { immediate: true },
)

function setChecked(name: string, value: boolean) {
  const next = new Set(checked.value)
  if (value) next.add(name)
  else next.delete(name)
  checked.value = next
}

const coreRow = computed(() => props.components.find((c) => c.kind === 'core'))
const coreChecked = computed(() => !!coreRow.value && checked.value.has(coreRow.value.name))
// A newer core exists and stays unchecked: what follows is the warning that
// comes *before* the refusal screen a mismatched protocol produces — that
// screen is the backstop, this is the earlier word.
const coreLeftBehind = computed(() => coreRow.value?.availability === 'update_available' && !coreChecked.value)
// The plugins of ours that have an update and stay unchecked: what the core
// row warns about when it is checked without them.
const pluginsLeftBehind = computed(() =>
  relevant.value
    .filter((c) => c.kind === 'plugin' && !c.third_party_repo)
    .filter((c) => c.availability === 'update_available' && !checked.value.has(c.name))
    .map((c) => c.name),
)

/** `null` when a row has nothing to say. */
function warningFor(c: ComponentOffer): string | null {
  // Fix round 1, M1: `installable === false` names a component whose archive
  // needs a root step this dialog cannot take — for a privileged plugin
  // (`files` today), an update whose companion (`files-mount`, the root-run
  // mount helper with its unit and polkit rule) moved, which only
  // ritornello-install places. An installed privileged plugin is NOT
  // refused from its name; the core leaves `installable` unset until an
  // attempt has been refused, so the switch follows the flag and nothing
  // else. The switch is disabled below for the
  // same reason `offered === null` already disables one (Ruling 88); this
  // is the sentence that tells the operator why, the same one `ConfigView`'s
  // table and `InstallablesDialog` show in place of a button.
  if (c.installable === false) {
    // The core says which companion, when that is the reason: the sentence
    // then tells the operator what to do about this very update, rather
    // than restating the privileged plugin's general rule.
    if (c.needs_companion) {
      return t.value('update_row_needs_companion', { companion: c.needs_companion })
    }
    // A stranger's plugin or a pack was refused for its archive, not for
    // being privileged (`refusedNote`).
    return refusedNote(t.value, c)
  }
  // A third-party pack: its label already names the source.
  if (c.kind === 'language_pack' && c.third_party_repo) return t.value('update_row_third_party_pack')
  if (c.kind === 'third_party' || c.third_party_repo) {
    return t.value('update_row_third_party', { repo: c.third_party_repo ?? '?' })
  }
  if (c.kind !== 'core' && checked.value.has(c.name) && coreLeftBehind.value) {
    return t.value('update_row_core_not_selected', { component: c.name })
  }
  // The symmetrical case: the core is ticked and plugins of ours that have
  // an update are not. Across a wire break, this core refuses each of them
  // until it is updated too, so the page would turn red row by row with no
  // earlier word. Our plugins only: a third-party row is never ticked by
  // default and already carries its own warning, so counting it here would
  // make this one a permanent fixture of the core row.
  if (c.kind === 'core' && checked.value.has(c.name) && pluginsLeftBehind.value.length > 0) {
    return t.value('update_row_plugins_not_selected', { components: pluginsLeftBehind.value.join(', ') })
  }
  return null
}

/**
 * What a row is called. A language pack is named by its language and its
 * source — a third party's id is a digest (`ritornello-xlang-fr-<h12>`) and
 * ours is an archive name, neither of which an operator should have to read.
 * Every other row keeps its component name.
 */
function labelFor(c: ComponentOffer): string {
  if (c.kind !== 'language_pack') return c.name
  const code = packLanguage(c.name)
  if (code === null) return c.name
  const language = languageName(code)
  return c.third_party_repo
    ? t.value('update_row_pack_third_party', { language, repo: c.third_party_repo })
    : t.value('update_row_pack_official', { language })
}

interface Row {
  offer: ComponentOffer
  label: string
  checked: boolean
  warning: string | null
}

const rows = computed<Row[]>(() =>
  relevant.value.map((offer) => ({
    offer,
    label: labelFor(offer),
    checked: checked.value.has(offer.name),
    warning: warningFor(offer),
  })),
)

const hasSelection = computed(() => checked.value.size > 0)

function confirm() {
  emit('confirm', [...checked.value])
}
</script>

<template>
  <Dialog :open="open" @update:open="(v) => emit('update:open', v)">
    <DialogContent data-update-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('update_dialog_title') }}</DialogTitle>
        <DialogDescription>{{ t('update_dialog_description') }}</DialogDescription>
      </DialogHeader>

      <ul class="space-y-3">
        <li
          v-for="row in rows"
          :key="row.offer.name"
          data-update-row
          :data-name="row.offer.name"
          class="flex items-start gap-2"
        >
          <!-- Ruling 88: guard on `offered === null`, never on `kind`. A row
               with no offered version cannot be selected for install, whoever
               published it — it catches a third-party row whose repository
               was over the cap, down or unaddressable, and it equally catches
               an official plugin this release does not carry. Disabling every
               third-party row would have also switched off the case task 17
               made real: a third-party plugin **with** an offer, which can
               now genuinely be updated from its own repository.

               Fix round 1, M1: `installable === false` disables it too, the
               same idiom as the missing-offer case above rather than hiding
               the row — `defaultChecked` already left it unticked, but the
               switch itself stayed enabled and a press-then-confirm reached
               `installable_from_ui`'s refusal regardless. -->
          <Switch
            data-update-row-check
            :model-value="row.checked"
            :disabled="row.offer.offered === null || row.offer.installable === false"
            :aria-label="row.label"
            @update:model-value="(v: boolean) => setChecked(row.offer.name, v)"
          />
          <div class="grid gap-0.5 text-sm">
            <span data-update-row-name>{{ row.label }}</span>
            <span v-if="row.warning" data-update-row-warning class="text-xs text-muted-foreground">
              {{ row.warning }}
            </span>
          </div>
        </li>
      </ul>

      <Button data-update-confirm :disabled="!hasSelection" @click="confirm">
        {{ t('update_confirm') }}
      </Button>
    </DialogContent>
  </Dialog>
</template>
