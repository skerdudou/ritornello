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
 * What is out of step and nothing else: `update_available`, never
 * third-party (its own repository decides, not this release), and never a
 * component already known to need a manual step (checking it again would
 * only repeat the same refusal).
 */
function defaultChecked(components: ComponentOffer[]): Set<string> {
  return new Set(
    components
      .filter((c) => c.kind !== 'third_party')
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
    if (open) checked.value = defaultChecked(props.components)
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

/** `null` when a row has nothing to say. */
function warningFor(c: ComponentOffer): string | null {
  if (c.kind === 'third_party') {
    return t.value('update_row_third_party', { repo: c.third_party_repo ?? '?' })
  }
  if (c.kind !== 'core' && checked.value.has(c.name) && coreLeftBehind.value) {
    return t.value('update_row_core_not_selected', { component: c.name })
  }
  return null
}

interface Row {
  offer: ComponentOffer
  checked: boolean
  warning: string | null
}

const rows = computed<Row[]>(() =>
  props.components.map((offer) => ({
    offer,
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
               now genuinely be updated from its own repository. -->
          <Switch
            data-update-row-check
            :model-value="row.checked"
            :disabled="row.offer.offered === null"
            :aria-label="row.offer.name"
            @update:model-value="(v: boolean) => setChecked(row.offer.name, v)"
          />
          <div class="grid gap-0.5 text-sm">
            <span data-update-row-name>{{ row.offer.name }}</span>
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
