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
import { contractLabel, fmt, refusalText } from '../composables/contractText'
import { languageName } from '../composables/languages'
import { defaultSelection, refusedByNewCore } from '../composables/majorUpdate'
import { packLanguage } from '../composables/packSource'
import { refusedNote } from '../composables/refusedNote'
import { useCatalog } from '../composables/useCatalog'
import type { ComponentOffer, ContractGap, Fit } from '../types'

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

// What the dialog ticks by itself (`defaultSelection`, shared with the card).
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
    if (open) checked.value = defaultSelection(relevant.value)
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
// The offered core breaks the wire and is ticked: a **major update**. The
// banner says so, and names what that core will refuse once it runs.
const majorUpdate = computed(() => coreRow.value?.breaking === true && coreChecked.value)
const refusedAfterMajor = computed(() =>
  majorUpdate.value ? refusedByNewCore(props.components, checked.value) : [],
)
// The plugins of ours that have an update and stay unchecked: what the core
// row warns about when it is checked without them.
const pluginsLeftBehind = computed(() =>
  relevant.value
    .filter((c) => c.kind === 'plugin' && !c.third_party_repo)
    .filter((c) => c.availability === 'update_available' && !checked.value.has(c.name))
    .map((c) => c.name),
)

/**
 * The verdict a plugin row's offered version gets from the core it will
 * actually meet, as the core judged it — never re-judged here.
 *
 * `with_core` is the verdict against the core the release offers (or the
 * running one when the release offers none), which is the core this row
 * meets **with the core ticked** — the state the dialog describes. Only
 * when a newer core is on offer and left unticked does the row meet the
 * running core instead, and `with_running_core` is then the verdict that
 * holds — for a ticked row only: with neither the core nor the row ticked,
 * nothing moves and there is nothing to judge.
 */
function fitFor(c: ComponentOffer): { fit: Fit; running: boolean } | null {
  if (coreLeftBehind.value) {
    if (!checked.value.has(c.name) || !c.with_running_core) return null
    return { fit: c.with_running_core, running: true }
  }
  return c.with_core ? { fit: c.with_core, running: false } : null
}

/** `source 2.0 vs 1.3`, one per gap, joined. */
function gapsText(gaps: ContractGap[]): string {
  return gaps
    .map((g) =>
      t.value('update_row_fit_gap', { contract: contractLabel(t.value, g.contract), found: fmt(g.plugin), expected: fmt(g.core) }),
    )
    .join(' · ')
}

/**
 * The second line of a plugin row: what its offered version becomes with
 * the core it will meet. `null` for a compatible one, which has nothing to
 * say. One literal key per branch (`i18nKeysUsed`).
 */
function fitLineFor(c: ComponentOffer): string | null {
  const judged = fitFor(c)
  if (!judged) return null
  const { fit, running } = judged
  if (fit.fit === 'limited') return t.value('update_row_fit_limited', { contracts: gapsText(fit.gaps) })
  if (fit.fit === 'refused') {
    const reason = refusalText(t.value, fit.refusal)
    // A ticked plugin with the core left unticked is installed alone (the
    // worker groups it with the core only when the core is part of the
    // gesture), and the core that keeps running refuses it: say so on the
    // row, with what ticking the core would change.
    return running
      ? t.value('update_row_fit_refused_unless_core', { reason })
      : t.value('update_row_fit_refused', { reason })
  }
  return null
}

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
    // The release carrying this archive does not say what it speaks: the
    // device cannot know whether the core would accept it, and never
    // assumes so.
    if (c.not_installable_reason === 'contracts_unpublished') {
      return t.value('update_row_contracts_unpublished', { component: c.name })
    }
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
    // The precise word, when the core already judged it: the fit line says
    // this row will be refused unless the core is ticked too, and the
    // general "may" would only repeat it less exactly.
    if (c.with_running_core?.fit === 'refused') return null
    return t.value('update_row_core_not_selected', { component: c.name })
  }
  // A major update without this plugin's own: the core that will run
  // refuses what stays installed, since its new version is the one built
  // for that core (`with_running_core` refused is the core's own definition
  // of a dependent).
  if (
    c.kind !== 'core' &&
    !checked.value.has(c.name) &&
    majorUpdate.value &&
    c.with_running_core?.fit === 'refused'
  ) {
    return t.value('update_row_refused_until_updated')
  }
  // The symmetrical case: the core is ticked and plugins of ours that have
  // an update are not. Across a wire break, this core refuses each of them
  // until it is updated too, so the page would turn red row by row with no
  // earlier word. Our plugins only: a third-party row is never ticked by
  // default and already carries its own warning, so counting it here would
  // make this one a permanent fixture of the core row.
  //
  // A breaking core says it in the banner instead, with the precise list of
  // what it will refuse, and each such row says so on its own line.
  if (
    c.kind === 'core' &&
    checked.value.has(c.name) &&
    !c.breaking &&
    pluginsLeftBehind.value.length > 0
  ) {
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
  fit: string | null
}

const rows = computed<Row[]>(() =>
  relevant.value.map((offer) => ({
    offer,
    label: labelFor(offer),
    checked: checked.value.has(offer.name),
    warning: warningFor(offer),
    fit: offer.kind === 'core' ? null : fitLineFor(offer),
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

      <!-- Between the header and the list: the core and some plugins change
           how they talk, and what is not updated with it is refused until it
           is. Shown only for a breaking core that is ticked — unticked, the
           running core stays and nothing breaks. -->
      <div
        v-if="majorUpdate"
        data-update-major
        role="note"
        class="rounded-md border border-border p-3 text-sm"
      >
        <p>{{ t('update_major_banner') }}</p>
        <p v-if="refusedAfterMajor.length > 0" data-update-major-refused class="text-xs text-muted-foreground">
          {{ t('update_major_refused', { components: refusedAfterMajor.join(', ') }) }}
        </p>
      </div>

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
            <span v-if="row.fit" data-update-row-fit class="text-xs text-muted-foreground">
              {{ row.fit }}
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
