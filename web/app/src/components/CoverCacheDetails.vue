<script setup lang="ts">
import {
  api,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@ritornello/ui'
import { ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { CachePayload } from '../types'

/**
 * What the cover cache really holds, behind a `(?)` next to the estimate.
 *
 * The estimate above the settings card *predicts* what a setting change
 * would do, from a model measured on one library. This panel reads the
 * actual snapshot the core keeps (`GET /api/cover-cache`), so the two can be
 * compared instead of trusted — the real average weight of a retained
 * thumbnail is the figure that matters here.
 *
 * **Read at the moment of opening, and again only on demand** (the reload
 * button) — never on a timer. A periodic refresh would repeat the fault
 * measured on the MPD side, where the server woke its clients once a second
 * for nothing.
 *
 * The catalog itself is not reloaded here: `t` reads the same module-level
 * ref `ConfigView.vue` already populates on its own mount (see
 * `useCatalog.ts`), same convention as `ProvenanceDetails.vue` and
 * `PlayerCard.vue`. Vue's reactivity does not care about mount order — once
 * the parent's fetch resolves, this button's label recomputes and rerenders
 * — and the dialog's own content only ever renders once opened, by which
 * time that fetch is long done. Reloading it here too would fire a second,
 * needless `/api/i18n` request on every visit to the settings page.
 */
const { t } = useCatalog()

const snapshot = ref<CachePayload | null>(null)
const error = ref(false)

async function load(): Promise<void> {
  error.value = false
  try {
    // `api.get` **throws** on failure, unlike `api.put`: without this `catch`
    // the rejection would go unhandled and the panel would stay mute.
    snapshot.value = await api.get<CachePayload>('/api/cover-cache')
  } catch {
    snapshot.value = null
    error.value = true
  }
}

/**
 * `Dialog` stays **mounted** when closed (see `ShareDialog.vue`): without
 * this reset on `@update:open`, the previous reading would flash back at the
 * next opening while the new one is still in flight.
 */
function onOpenChange(open: boolean): void {
  if (!open) return
  snapshot.value = null
  error.value = false
  void load()
}

/**
 * Real average weight of one retained thumbnail, in KiB, rounded — `null`
 * rather than `NaN` or `Infinity` when the cache holds none.
 */
function averageKio(s: CachePayload): number | null {
  if (s.renditions <= 0) return null
  return Math.round(s.renditions_bytes / s.renditions / 1024)
}

/** Bytes never display raw ("12582912" informs nobody): mebibytes, rounded. */
function mio(bytes: number): number {
  return Math.round(bytes / 1024 / 1024)
}
</script>

<template>
  <Dialog @update:open="onOpenChange">
    <DialogTrigger as-child>
      <!-- Same affordance as ProvenanceDetails.vue, copied rather than
           reinvented: 44 px round ghost target, same question-mark icon. -->
      <Button
        variant="ghost"
        class="relative z-10 size-11 shrink-0 rounded-full text-muted-foreground"
        :aria-label="t('cover_cache_open')"
        :title="t('cover_cache_open')"
        data-cover-cache-open
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
          <circle cx="12" cy="12" r="9" />
          <path d="M9.6 9.2a2.5 2.5 0 1 1 3.2 3.4c-.5.3-.8.8-.8 1.4v.4" />
          <path d="M12 17.4h.01" />
        </svg>
      </Button>
    </DialogTrigger>
    <DialogContent data-cover-cache-panel>
      <DialogHeader>
        <DialogTitle>{{ t('cover_cache_title') }}</DialogTitle>
        <!-- Not decorative: reka-ui ties it in via `aria-describedby`, and its
             absence leaves a screen reader announcing a dialog it can say
             nothing about. -->
        <DialogDescription>{{ t('cover_cache_hint') }}</DialogDescription>
      </DialogHeader>

      <p v-if="error" class="text-sm text-destructive" data-cover-cache-error>
        {{ t('cover_cache_failed') }}
      </p>

      <template v-else-if="snapshot">
        <!-- A definition list and not a table: two columns, one of which
             fits in a single word, on a panel that must stay readable on
             the phone. -->
        <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt class="text-muted-foreground">{{ t('cover_cache_used') }}</dt>
          <dd class="font-medium">{{ mio(snapshot.used_bytes) }} / {{ mio(snapshot.budget_bytes) }}</dd>

          <dt class="text-muted-foreground">{{ t('cover_cache_entries') }}</dt>
          <dd class="font-medium">
            {{ snapshot.entries }}
            <span class="font-normal text-muted-foreground">
              ({{ snapshot.entries_free }} {{ t('cover_cache_entries_free') }})
            </span>
          </dd>

          <dt class="text-muted-foreground">{{ t('cover_cache_renditions') }}</dt>
          <dd class="font-medium">{{ snapshot.renditions }}</dd>

          <!-- The line that matters: the real average weight, confronted
               against the predicted weight shown on the settings card. -->
          <template v-if="averageKio(snapshot) !== null">
            <dt class="text-muted-foreground">{{ t('cover_cache_average') }}</dt>
            <dd class="font-medium" data-cover-cache-average>{{ averageKio(snapshot) }}</dd>
          </template>

          <dt class="text-muted-foreground">{{ t('cover_cache_stale') }}</dt>
          <dd class="font-medium">{{ snapshot.renditions_stale }}</dd>

          <!-- The only place `max_entries` is ever shown, and shown as what
               it is: a bound on a **count**, not on bytes. The settings page
               stays silent about it. -->
          <dt class="text-muted-foreground">{{ t('cover_cache_belt') }}</dt>
          <dd class="font-medium">{{ snapshot.max_entries }}</dd>
        </dl>

        <p v-if="snapshot.renditions === 0" class="text-sm text-muted-foreground">
          {{ t('cover_cache_empty') }}
        </p>
      </template>

      <Button variant="secondary" class="justify-self-start" data-cover-cache-reload @click="load">
        {{ t('reload') }}
      </Button>
    </DialogContent>
  </Dialog>
</template>
