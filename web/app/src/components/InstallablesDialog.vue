<script setup lang="ts">
import {
  api, Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle,
} from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { ComponentOffer } from '../types'

const props = defineProps<{ open: boolean; components: ComponentOffer[] }>()
const emit = defineEmits<{ 'update:open': [boolean]; install: [string] }>()
const { t } = useCatalog()

/** What the release says about its components, `null` until asked. */
const catalogue = ref<Record<string, { kinds: string[]; description: string }> | null>(null)
const asked = ref(false)

/**
 * Components the release publishes and this device does not have.
 *
 * Modelled as "an installable component, with a category" rather than as
 * "a plugin": a second category — installable language packs, or plugins
 * from a declared third-party source — must be able to join as a section
 * rather than as a second dialog. Nothing of either is written here.
 */
const rows = computed(() =>
  props.components
    .filter((c) => c.availability === 'not_installed')
    .map((c) => ({ offer: c, entry: catalogue.value?.[c.name] ?? null })),
)

/** The release published no catalogue at all — distinguished from "this
 *  device has everything", which is `rows.length === 0`. */
const noCatalogue = computed(
  () => asked.value && Object.keys(catalogue.value ?? {}).length === 0 && rows.value.length > 0,
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
    // life of its session, keyed by release tag, and re-asking would
    // defeat that from this side.
    asked.value = true
    const answer = await api
      .get<{ components: Record<string, { kinds: string[]; description: string }> }>(
        '/api/update/catalogue',
      )
      .catch((e: unknown) => {
        // An unreachable catalogue must not close the dialog: names alone
        // stay useful, and installing does not depend on a description.
        console.warn('update catalogue unavailable', e)
        return { components: {} }
      })
    if (localGeneration === generation) catalogue.value = answer.components
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

      <p v-if="rows.length === 0" data-installables-empty class="text-sm text-muted-foreground">
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
            <!-- Never disabled for a missing description: installing does not
                 depend on knowing how to describe the component. -->
            <Button
              variant="outline" size="xs" data-installable-install
              @click="emit('install', row.offer.name)"
            >{{ t('installables_install') }}</Button>
          </li>
        </ul>
      </template>
    </DialogContent>
  </Dialog>
</template>
