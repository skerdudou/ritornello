<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { ReloadIcon } from '@radix-icons/vue'
import { Badge, Card, CardAction, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import ProgressBar from './ProgressBar.vue'
import ProvenanceDetails from './ProvenanceDetails.vue'
import AppleMusicIcon from './icons/AppleMusicIcon.vue'
import DeezerIcon from './icons/DeezerIcon.vue'
import YoutubeIcon from './icons/YoutubeIcon.vue'
import { LINK_LABEL } from './links'
import { useCatalog } from '../composables/useCatalog'
import { formatDuration, nothingToShow } from '../composables/usePlayer'
import type { PlayerPayload } from '../types'

// The state comes from the parent (HomeView), which holds the page's **single**
// SSE connection: the remote needs it too (active key), and opening a second
// connection here would double the streams for the same content.
const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null; seekStep: number }>()
// The device announced a cover, the browser could not load it.
//
// The case is not theoretical: the core's cache key is capped at a few
// entries, and the file itself lives on a share that can vanish — both yield
// a 404 under an already-published URL. Without this flag, the reserved square
// showed the browser's broken-image glyph instead of the ♫ fallback intended
// for exactly this situation.
const imageBroken = ref(false)

/**
 * Number of retries granted to an announced image, before the ♫ fallback.
 *
 * **A failure is no longer final for the track**, and that is the point. The
 * owner reports covers that do not load, some of which end up arriving "much
 * later": publishing the URL and the actual availability of the bytes are not
 * the same instant, and the first `error` of the `<img>` doomed the square
 * until the next track. Two spaced-out retries catch a transient dip without
 * hammering the device.
 */
const IMAGE_RETRIES = 2
/** Delay before each retry, in milliseconds: short, then less short. */
const RETRY_DELAYS_MS = [800, 3000]
/** How many retries have already been consumed for the current URL. */
const retriesDone = ref(0)
/**
 * Counter appended to the URL on retries.
 *
 * Without it the browser would serve back its own cached failure: a 404
 * response is cacheable, and requesting the same URL again would not go back
 * to the network. It only moves on retries, so the nominal case keeps a stable
 * URL and the browser cache plays its role.
 */
const attempt = ref(0)
let retryTimer: ReturnType<typeof setTimeout> | null = null

function cancelRetry() {
  if (retryTimer !== null) {
    clearTimeout(retryTimer)
    retryTimer = null
  }
}

/**
 * The `<img>` failed: retry if the budget allows, otherwise fall back.
 *
 * **The ♫ fallback is shown in both cases**, immediately. Leaving the `<img>`
 * in place while waiting would render the browser's broken-image glyph —
 * exactly what `imageBroken` exists to avoid — and a retry would make it
 * flicker. So the square shows the fallback, and the image comes back on its
 * own if the retry succeeds.
 */
function onImageError() {
  imageBroken.value = true
  if (retriesDone.value >= IMAGE_RETRIES) return
  const delay = RETRY_DELAYS_MS[retriesDone.value] ?? 3000
  retriesDone.value += 1
  cancelRetry()
  retryTimer = setTimeout(() => {
    retryTimer = null
    // Order matters: the new URL first, the remount second. The other way
    // round, the `<img>` would reappear for an instant with the URL that just
    // failed, and the browser would serve back its cached failure.
    attempt.value += 1
    imageBroken.value = false
  }, delay)
}

// Reset as soon as the device points at a **different** image: otherwise a
// single failure would doom the square for the rest of the session.
watch(
  () => props.state?.cover_href,
  () => {
    imageBroken.value = false
    // The retry budget is **per image**: a new URL starts over with its own,
    // and the previous one's timer no longer has a purpose.
    retriesDone.value = 0
    attempt.value = 0
    cancelRetry()
    // An enlarged cover left open while the track changes would show the next
    // track's image full screen, without anyone asking for it. Closing is the
    // only honest answer.
    enlarged.value = false
    enlargedLoading.value = false
  },
)
// True when the device announces an image and the browser managed to load it:
// the only condition under which the square is clickable.
const hasImage = computed(() => !!props.state?.cover_href && !imageBroken.value)
/**
 * The URL of the **thumbnail**, the one the card's square displays.
 *
 * The square is 224 px on a phone; loading a NAS's `folder.jpg` into it —
 * commonly two or three mebibytes — was pure waste, especially over Wi-Fi.
 * The core knows how to produce the reduced version (it is the one it already
 * pushes to the displays), it just has to be asked. The bare URL stays the
 * image as it is, and that is what the enlarged view loads.
 */
const thumbnailHref = computed(() => {
  if (!props.state?.cover_href) return null
  const base = `${props.state.cover_href}?size=thumbnail`
  // `attempt` only appears from the first retry onwards: see its doc.
  return attempt.value === 0 ? base : `${base}&attempt=${attempt.value}`
})
/** Is the cover open full screen? */
const enlarged = ref(false)
/**
 * Is the full-size image still on its way?
 *
 * Deliberately its own flag, not `imageBroken`: that one describes the
 * **thumbnail**, and sharing it would let a slow or unavailable full size
 * condemn the player's square, which did nothing wrong. The core now falls
 * back to serving the thumbnail bytes under the same URL when the full size
 * cannot be fetched (Task 8), so there is no failure state left to render
 * here — only a wait that always ends in a `load` event.
 */
const enlargedLoading = ref(false)
/**
 * Opens the full-screen view and arms the loading indicator.
 *
 * Set unconditionally on every open, not only the first one: the full size
 * is now fetched on demand (Task 8), so a second look at the same track can
 * be just as slow as the first if the core's cache has since evicted it.
 */
function openEnlarged() {
  enlarged.value = true
  enlargedLoading.value = true
}
// Escape closes, like any modal overlay. The listener only exists while open:
// a permanent global listener for a rarely-opened view is a debt, and it would
// catch keys on pages that have no cover at all.
function onEscape(e: KeyboardEvent) {
  if (e.key === 'Escape') enlarged.value = false
}
watch(enlarged, (open) => {
  if (open) window.addEventListener('keydown', onEscape)
  else window.removeEventListener('keydown', onEscape)
})
// Without this, leaving the page with the cover open leaves the listener
// behind — and the retry timer would run against an unmounted component.
onUnmounted(() => {
  window.removeEventListener('keydown', onEscape)
  cancelRetry()
})
// The duration is only shown when there is no progress bar: when a position is
// known, the bar already carries the total duration.
const durationToShow = computed(
  () => props.state?.position_s == null && !!formatDuration(props.state?.duration_s),
)
// The links this version knows how to render. The protocol closes the set of
// platforms, but a plugin ahead of the UI may name a new one: letting it
// through would give a 44 px anchor with no icon and no accessible name
// (`LINK_LABEL` would have no entry for it). Filter here rather than attempt a
// default rendering, which would announce "Listen on Apple Music" for a link
// that does not lead there.
const links = computed(
  () => props.state?.links?.filter((link) => link.platform in LINK_LABEL) ?? [],
)
// Provenance has something to say as soon as the core has named a field or an
// empty-handed contributor. This decides the presence of the `(?)`, hence that
// of the row when nothing else occupies it.
const hasOrigins = computed(() => {
  const p = props.state?.provenance
  return Object.keys(p?.fields ?? {}).length > 0 || (p?.misses?.length ?? 0) > 0
})
// The bottom row of the track block (provenance, duration, links) only exists
// if there is something to put in it: otherwise `min-h-11` would reserve 44
// empty px under the album, which is the most common case (a bare ICY title).
const badgeRow = computed(
  () => hasOrigins.value || durationToShow.value || links.value.length > 0,
)
// Bubbled up to the parent: HomeView is what posts the commands (as for the
// rest of the remote), the card itself posts none.
const emit = defineEmits<{ seek: [seconds: number] }>()
</script>

<template>
  <!--
    The cover and the track are the subject: they are the only thing one looks
    at from the couch. The state (source, standby) fits in the header; the
    volume is the slider in the `commandes` slot. On a phone everything is
    centered in a column; from `md` up the cover moves to the left of the text.
  -->
  <Card data-player>
    <CardHeader class="pb-2">
      <CardTitle class="flex items-center gap-2 text-base">
        {{ t('player_title') }}
        <!-- The source as a pill: a kit badge, `data-source` kept for the
             journeys. The dot says "it's playing" (playback), where the old
             text line said nothing.

             `bg-current` and not `bg-primary`: the dot inherits the badge's
             text color, so it contrasts **by construction** with its own
             background, in every theme. With `bg-primary` it painted the
             theme's green over the secondary badge's blue — two saturated,
             close hues, reported unreadable by the owner. It is also the
             idiom already chosen for the active preset pill (see
             `PresetGrid.vue`). The color carries no meaning here anyway: it
             is the **presence** of the dot that says it is playing, it is
             only rendered at that moment. -->
        <Badge variant="secondary" class="gap-1.5 font-normal">
          <span
            v-if="state?.playback === 'playing'"
            class="size-1.5 rounded-full bg-current"
            aria-hidden="true"
            data-now-playing-line
          />
          <span data-source>{{ state ? state.source || t('no_source') : '' }}</span>
        </Badge>
        <Badge v-if="state?.standby" variant="secondary" data-standby>{{ t('standby') }}</Badge>
      </CardTitle>
      <CardAction v-if="$slots.actions">
        <slot name="actions" />
      </CardAction>
    </CardHeader>
    <CardContent class="flex flex-col items-center gap-4 md:flex-row md:items-start md:gap-5">
      <!-- The square is always there, image or fallback: it is what holds the
           layout, and an image arriving after the text must shift nothing.
           224 px on a phone (the subject), 176 px next to the text on a PC. -->
      <div
        class="size-56 shrink-0 overflow-hidden rounded-lg border border-border bg-muted shadow-md md:size-44"
        :class="{ 'opacity-50': state?.standby }"
        data-cover-image
      >
        <!-- A real button and not a `@click` on the image: the enlarged view
             then also opens from the keyboard and carries an accessible name.
             There is none when there is nothing to enlarge — the ♫ fallback is
             not an image, and a button that opens nothing is worse than no
             button. -->
        <button
          v-if="hasImage"
          type="button"
          class="size-full cursor-zoom-in focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
          :aria-label="t('cover_zoom')"
          :title="t('cover_zoom')"
          data-cover-enlarge
          @click="openEnlarged"
        >
          <img
            :src="thumbnailHref!"
            :alt="t('cover_alt')"
            class="size-full object-cover"
            @error="onImageError"
          />
        </button>
        <div
          v-else
          class="flex size-full items-center justify-center text-muted-foreground"
          data-cover-fallback
          aria-hidden="true"
        >
          <svg width="56" height="56" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M9 18V5l12-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="18" cy="16" r="3" />
          </svg>
        </div>
      </div>
      <div class="flex min-w-0 flex-1 flex-col items-center gap-1 text-center md:items-start md:text-left">
        <!-- The preset as an overline: `P1 · FIP`. Absent when the source
             declares none (cd without a disc, aux input). -->
        <p v-if="state?.preset != null" class="text-[11px] font-semibold uppercase tracking-wider text-primary">
          P<span data-player-preset>{{ state.preset }}</span>
          <template v-if="state.preset_name"> · <span data-player-preset-name>{{ state.preset_name }}</span></template>
        </p>
        <!-- The source's status ("PAS DE DISQUE"), hidden in standby: the
             STANDBY badge already carries the word. -->
        <p v-if="state?.status && !state.standby" class="text-sm text-muted-foreground" data-player-status>
          {{ state.status }}
        </p>
        <div v-if="!nothingToShow(state)" class="flex min-w-0 flex-col items-center gap-0.5 md:items-start" data-now-playing>
          <p v-if="state?.title" class="text-xl font-semibold leading-tight text-foreground" data-title>{{ state.title }}</p>
          <p v-if="state?.artist" class="text-sm text-foreground" data-artist>{{ state.artist }}</p>
          <!-- The year sits next to the album, where a year is read. It also
               stands alone: a stream may know it without knowing the album. -->
          <p v-if="state?.album || state?.year" class="text-sm text-muted-foreground">
            <span v-if="state?.album" data-album>{{ state.album }}</span>
            <span v-if="state?.album && state?.year"> · </span>
            <span v-if="state?.year" :title="t('release_year')" data-year>{{ state.year }}</span>
          </p>
          <!-- Who supplied the text, and the cover when it is not the same:
               the first question in front of a wrong title. The listening
               platforms share this row: a row of their own pushed the volume
               slider out of the thumb's reach on a phone. `min-h-11` reserves
               the touch target's height up front, otherwise a link arriving
               after the title (MusicBrainz answers later) would grow the card
               under the finger. The row only exists if there is something to
               put in it. -->
          <div
            v-if="badgeRow"
            class="mt-1 flex min-h-11 items-center gap-1.5"
            data-badges
          >
            <!-- The two origin badges gave way to this button (owner's
                 decision): they occupied the busiest row of the screen with
                 two words nobody reads while listening, and they did not even
                 answer the question one asks in front of a wrong title —
                 *which field* comes from *whom*. The detail now lives in a
                 popover, where there is room to spell it out. -->
            <ProvenanceDetails :state="state" />
            <span
              v-if="durationToShow"
              class="text-xs text-muted-foreground"
              :title="t('track_length')"
              data-duration
            >
              {{ formatDuration(state?.duration_s) }}
            </span>
            <!-- `platform` is a closed set on the protocol side and the URL
                 has already been validated against that platform's host:
                 nothing to revalidate here. `noopener` because the target is
                 a third party, `noreferrer` because it has no business
                 knowing where we come from. The key is the URL and not the
                 platform: nothing forbids two links from the same platform,
                 and Vue would lose one.
                 The anchor no longer carries a color itself (neither at rest
                 nor on hover): each icon already carries its hard-coded brand
                 color (owner's decision, an acknowledged exception to the "no
                 hard-coded color" rule, see docs/interface.md § Player card),
                 and a text tint on top would muddle it without adding
                 anything. `hover:opacity-80` keeps a perceptible hover
                 feedback despite the absence of a color change.
                 `relative z-10`: the 44 px hit area of ProgressBar's thumb
                 overflows 19 px above its track (see ProgressBar.vue), while
                 this row is only 8 px higher — the overflow therefore covers
                 the bottom of these anchors (real targets, unlike the
                 durations below the track). Moving them in front in the paint
                 order gives the tap back to the links: the thumb keeps its
                 whole lower hit area and at least 33 px at the top, plenty to
                 remain usable. -->
            <span v-if="links.length" class="relative z-10 inline-flex items-center gap-1" data-links>
              <a
                v-for="link in links"
                :key="link.url"
                :href="link.url"
                target="_blank"
                rel="noopener noreferrer"
                class="inline-flex size-11 items-center justify-center rounded-md transition-opacity hover:opacity-80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                :aria-label="t(LINK_LABEL[link.platform])"
                :title="t(LINK_LABEL[link.platform])"
                :data-link="link.platform"
              >
                <!-- No `v-else`: the three branches exhaust the set already
                     filtered by `links`, and a `v-else` would render the Apple
                     icon for everything else. -->
                <YoutubeIcon v-if="link.platform === 'youtube'" class="size-5" />
                <DeezerIcon v-else-if="link.platform === 'deezer'" class="size-5" />
                <AppleMusicIcon v-else-if="link.platform === 'apple_music'" class="size-5" />
              </a>
            </span>
          </div>
        </div>
      </div>
    </CardContent>
    <!-- The spacing around the progress bar was tightened at the owner's
         request, then **loosened by 4 px**: at `space-y-2` the durations row
         touched the commands, "glued to the pixel". `space-y-3` gives the tiny
         gap requested without going back to the previous airiness. The
         counterpart above the track lives in `ProgressBar.vue` (`-mt-3`), the
         `gap-6` of the kit's `Card` not being adjustable here. -->
    <CardContent class="space-y-3 pt-0">
      <ProgressBar
        :position="state?.position_s ?? null"
        :duration="state?.duration_s ?? null"
        :seekable="state?.seekable ?? false"
        :step="seekStep"
        @seek="(s) => emit('seek', s)"
      />
      <slot name="commandes" />
    </CardContent>
    <!-- The cover full screen. `Teleport` to the `body`: the card has an
         `overflow-hidden` (rounded corners) and its own stacking context, an
         overlay rendered inside it would have ended up clipped. A click
         **anywhere** closes, including on the image: that is the request
         ("close by clicking again"), and it is also what every image viewer
         does. -->
    <Teleport to="body">
      <div
        v-if="enlarged"
        class="fixed inset-0 z-50 flex cursor-zoom-out items-center justify-center bg-black/80 p-4"
        role="dialog"
        aria-modal="true"
        :aria-label="t('cover_alt')"
        data-cover-enlarged
        @click="enlarged = false"
      >
        <!-- `object-contain` and not `object-cover`: enlarging is precisely
             for seeing the whole cover, a crop would betray it. -->
        <img
          :src="state?.cover_href ?? ''"
          :alt="t('cover_alt')"
          class="max-h-full max-w-full rounded-lg object-contain shadow-2xl"
          @load="enlargedLoading = false"
        />
        <!-- The full size is now fetched on demand (Task 8): it can take a
             moment, and a plain dark veil with nothing in it reads as a bug.
             Painted after the `<img>` so it stacks above it while the bytes
             are still streaming in. `role="status"` announces the wording to
             a screen reader on its own (implicit `aria-live="polite"`),
             without a listener of its own to maintain. -->
        <div
          v-if="enlargedLoading"
          class="absolute inset-0 flex flex-col items-center justify-center gap-2 text-white"
          role="status"
          data-cover-enlarged-loading
        >
          <ReloadIcon class="size-8 animate-spin" aria-hidden="true" />
          <span>{{ t('cover_zoom_loading') }}</span>
        </div>
        <!-- The close button doubles the click on the backdrop, it does not
             replace it: without it, there is no way to close from the
             keyboard other than Escape, which is announced nowhere. -->
        <button
          type="button"
          class="absolute right-4 top-4 rounded-full bg-black/50 p-3 text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white"
          :aria-label="t('cover_zoom_close')"
          :title="t('cover_zoom_close')"
          data-cover-close
          @click.stop="enlarged = false"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </div>
    </Teleport>
  </Card>
</template>
