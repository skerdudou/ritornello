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
  HelpButton,
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
 * compared instead of trusted.
 *
 * **It answers one question — where does the memory go — and its shape is
 * that answer: two lines whose weights add up to the total.** It used to
 * report *mechanisms* instead (thumbnails re-encoded, thumbnails supplied,
 * full sizes downloaded), and the owner found the hole by looking at it while
 * playing a radio: all three read zero while the memory climbed, because a
 * cover announced as a single URL is none of the three. A panel where a held
 * byte belongs to no line cannot be read, whatever each line says.
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

/** One mebibyte, the threshold at which `weight` switches units. */
const MIO = 1024 * 1024

/**
 * Bytes as a phrase a reader can act on — never raw, since "12582912"
 * informs nobody.
 *
 * **The unit is chosen per value, and that is not cosmetic.** Fixed at
 * mebibytes, a handful of radio covers printed "0" beside a count that was
 * visibly not zero: the panel showed nothing occupied while it held several
 * hundred kibibytes, which is exactly the blindness this rewrite exists to
 * remove. Below a mebibyte the value is therefore given in kibibytes.
 *
 * The number goes **into** the translated string rather than in front of it:
 * a language is free to place its unit where it wants, and not all of them
 * put it last.
 */
function weight(bytes: number): string {
  return bytes < MIO
    ? t.value('cover_cache_kio', { n: Math.round(bytes / 1024) })
    : t.value('cover_cache_mio', { n: Math.round(bytes / MIO) })
}

/**
 * The budget is always stated in mebibytes, whatever the occupied side reads:
 * it is a setting the user typed in mebibytes, on the card just above, and
 * echoing it in another unit would stop the two from being comparable.
 */
function used(s: CachePayload): string {
  return t.value('cover_cache_used_value', {
    used: weight(s.used_bytes),
    budget: t.value('cover_cache_mio', { n: Math.round(s.budget_bytes / MIO) }),
  })
}
</script>

<template>
  <Dialog @update:open="onOpenChange">
    <DialogTrigger as-child>
      <!-- Same affordance as ProvenanceDetails.vue: the glyph now lives in
           `HelpButton`, so it is read from there rather than redrawn here.
           **`inline`, and no `z-10`, unlike ProvenanceDetails.** Those two
           were the crowding workaround this button needed at the foot of the
           card, where it shared a line with an estimate free to overflow into
           it. In a `CardHeader` it shares its line with a title and nothing
           else, so the 44 px target and the stacking rescue are both answers
           to a problem that no longer exists — and a 44 px box beside a card
           title reads as a second heading. -->
      <HelpButton :label="t('cover_cache_open')" data-cover-cache-open />
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
        <!-- **Read top-down: the summary, then its breakdown.** Work done
             first, since it is the only line that is not memory at all and
             would sit oddly in the middle of ones that are; then the total;
             then the two lines that make it up, heaviest first. The two do
             still add up to the total above them — the order changes where a
             reader meets the sum, not the arithmetic. -->
        <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <!-- Work done, not memory held. Cumulative since boot, so it
               answers "is this appliance re-encoding" rather than "what is it
               holding" — which is why it opens the list instead of joining
               the two lines that account for bytes. -->
          <dt class="text-muted-foreground">{{ t('cover_cache_renditions') }}</dt>
          <dd class="font-medium" data-cover-cache-renditions>{{ snapshot.renditions_built }}</dd>

          <dt class="text-muted-foreground">{{ t('cover_cache_used') }}</dt>
          <dd class="font-medium" data-cover-cache-used>{{ used(snapshot) }}</dd>

          <!-- The heaviest thing the cache can hold: a cover downloaded whole
               from the network, and every full size somebody enlarged. A
               station announcing a single URL lands here, and landing nowhere
               at all is what this panel used to do with it. -->
          <dt class="text-muted-foreground">{{ t('cover_cache_full_sizes') }}</dt>
          <dd class="font-medium" data-cover-cache-full>
            {{ snapshot.full_sizes }}
            <span v-if="snapshot.full_sizes > 0" class="font-normal text-muted-foreground">
              ({{ weight(snapshot.full_sizes_bytes) }})
            </span>
          </dd>

          <!-- Every thumbnail the cache holds, on one line: the ones a
               contributor supplied and the ones the encoder produced. They
               cost the same memory, which is the only thing this panel
               accounts for — where each came from is what the re-encoding
               count above tells, in one number, without splitting the
               accounting in two. -->
          <dt class="text-muted-foreground">{{ t('cover_cache_thumbnails') }}</dt>
          <dd class="font-medium" data-cover-cache-thumbnails>
            {{ snapshot.thumbnails }}
            <span v-if="snapshot.thumbnails > 0" class="font-normal text-muted-foreground">
              ({{ weight(snapshot.thumbnails_bytes) }})
            </span>
          </dd>
        </dl>

        <!-- **"Nothing in memory", which is not the same as "nothing at
             all".** A library of local covers holds hundreds of entries that
             cost a path and no bytes: they legitimately appear on neither
             line, and the sentence must not claim the cache is empty. Judged
             on the total rather than on either line, so it cannot contradict
             a figure printed just above it. -->
        <p v-if="snapshot.used_bytes === 0" class="text-sm text-muted-foreground">
          {{ t('cover_cache_empty') }}
        </p>
      </template>

      <Button variant="secondary" class="justify-self-start" data-cover-cache-reload @click="load">
        {{ t('reload') }}
      </Button>
    </DialogContent>
  </Dialog>
</template>
