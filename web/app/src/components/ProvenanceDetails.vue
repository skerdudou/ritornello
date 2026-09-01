<script setup lang="ts">
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@ritornello/ui'
import { computed } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'

/**
 * The metadata provenance detail, behind a `(?)`.
 *
 * **What it replaces, and why.** The card carried two badges — who provided
 * the text, who provided the cover — on the busiest line of the screen. Two
 * words for a piece of information nobody reads while looking at their music,
 * and which did not even answer the question one really asks in front of a
 * wrong title: *which* field comes from *whom*. The displayed text is composed
 * by several hands — the winner of the arbitration, the `fill_only` that fill
 * its gaps, the year and the links taken from any contributor, the cover that
 * often comes from elsewhere — and `origin` only named the first one.
 *
 * So the detail lives in a popin, where there is room to spell it out, and
 * the badges line gives its space back to the rest.
 */
const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null }>()

/**
 * The fields, in the order the screen shows them.
 *
 * A fixed order and not the map's: the map is sorted by field name (it is a
 * `BTreeMap` core side, so that the frame is stable), which would give
 * "album, artist, duration, title" — the alphabetical order of a dictionary,
 * not that of a player card.
 */
const ORDER = ['title', 'artist', 'album', 'year', 'duration', 'cover', 'links'] as const

/** The label of each field, by catalog key. */
const LABEL: Record<string, string> = {
  title: 'provenance_field_title',
  artist: 'provenance_field_artist',
  album: 'provenance_field_album',
  year: 'provenance_field_year',
  duration: 'provenance_field_duration',
  cover: 'provenance_field_cover',
  links: 'provenance_field_links',
}

const fields = computed(() => {
  const provided = props.state?.provenance?.fields ?? {}
  const derived = props.state?.provenance?.derived ?? {}
  return ORDER.filter((c) => provided[c]).map((c) => ({
    field: c,
    by: provided[c]!,
    // The plugin that reworked this field without being its source — the
    // splitting of an ICY header, typically. Shown **next to** the source,
    // never in its place: that is the whole point of the distinction.
    derivedBy: derived[c],
  }))
})

const misses = computed(() => props.state?.provenance?.misses ?? [])

/**
 * The button only exists if there is something to say.
 *
 * A `(?)` opening an empty popin would be worse than no button: it promises an
 * explanation and gives none. This is the ordinary case before a track gets
 * identified.
 */
const hasSomethingToSay = computed(() => fields.value.length > 0 || misses.value.length > 0)
</script>

<template>
  <Dialog v-if="hasSomethingToSay">
    <DialogTrigger as-child>
      <!-- `size-11`: the recommended 44 px touch target, on a line where the
           progress bar's slider already overflows (see PlayerCard.vue).
           `relative z-10` for the same reason as the neighbouring platform
           links — getting in front of that overflow gives the tap back. -->
      <Button
        variant="ghost"
        class="relative z-10 size-11 shrink-0 rounded-full text-muted-foreground"
        :aria-label="t('provenance_open')"
        :title="t('provenance_open')"
        data-provenance-open
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
          <circle cx="12" cy="12" r="9" />
          <path d="M9.6 9.2a2.5 2.5 0 1 1 3.2 3.4c-.5.3-.8.8-.8 1.4v.4" />
          <path d="M12 17.4h.01" />
        </svg>
      </Button>
    </DialogTrigger>
    <DialogContent data-provenance-popover>
      <DialogHeader>
        <DialogTitle>{{ t('provenance_title') }}</DialogTitle>
        <DialogDescription>{{ t('provenance_hint') }}</DialogDescription>
      </DialogHeader>

      <!-- A definition list and not a table: two columns, one of which fits in
           a single word, on a popin that must stay readable on the phone. -->
      <dl v-if="fields.length" class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <template v-for="c in fields" :key="c.field">
          <dt class="text-muted-foreground">{{ t(LABEL[c.field]!) }}</dt>
          <dd class="font-medium" :data-provenance-field="c.field">
            {{ c.by }}
            <span
              v-if="c.derivedBy"
              class="font-normal text-muted-foreground"
              :data-provenance-derived="c.field"
            >{{ t('provenance_derived_by', { par: c.derivedBy }) }}</span>
          </dd>
        </template>
      </dl>

      <!-- Those who searched without finding anything. A separate section, and
           not a "—" line in the list above: "musicbrainz has no album for this
           track" is not "musicbrainz was never queried", and that is precisely
           the distinction just added to the protocol. -->
      <div v-if="misses.length" class="space-y-1" data-provenance-misses>
        <p class="text-sm text-muted-foreground">{{ t('provenance_misses') }}</p>
        <div class="flex flex-wrap gap-1.5">
          <Badge v-for="m in misses" :key="m" variant="secondary" class="font-normal">{{ m }}</Badge>
        </div>
      </div>
    </DialogContent>
  </Dialog>
</template>
