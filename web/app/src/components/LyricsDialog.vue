<script setup lang="ts">
import {
  api,
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@ritornello/ui'
import { computed, nextTick, ref, watch } from 'vue'
import MicIcon from './icons/MicIcon.vue'
import { activeLine, MANUAL_SCROLL_HOLD_MS } from '../composables/lyrics'
import { useCatalog } from '../composables/useCatalog'
import type { RenderedLyrics } from '../types'

/**
 * The lyrics of what is playing, behind a microphone on the card's badge row.
 *
 * **The text is fetched when the dialog opens, never before.** Lyrics are
 * read by someone who asked for them; fetching them on every track change
 * would cost the device a request per song for a dialog almost nobody opens.
 * The URL is immutable (the key names the content), so a second open of the
 * same song is answered by the browser's cache.
 *
 * **The dialog survives the lyrics.** When the song changes under an open
 * dialog, the new song's lyrics replace the old ones; when the new song has
 * none, the dialog says so and stays open. Closing it on the listener's
 * behalf would be the dialog deciding it knows better than the hand that
 * opened it — and the next song may well bring lyrics back.
 */
const { t } = useCatalog()
const props = defineProps<{
  /** `/api/lyrics/{key}`, or `null` when nothing is known for this song. */
  href: string | null
  /** The lyrics carry timed lines. */
  synced: boolean
  /** Where the track stands, in seconds; `null` when nobody knows (a radio). */
  positionS: number | null
  /** Who provided the lyrics, named in the footer. */
  origin: string | null
}>()
/**
 * Whether the dialog is open, shared with the card: the card keeps the row
 * that holds this component while the dialog is open, otherwise a song with
 * nothing to show would unmount the dialog from under the listener.
 */
const open = defineModel<boolean>('open', { default: false })

/** The fetched lyrics, for `loadedHref`; `null` while none are loaded. */
const lyrics = ref<RenderedLyrics | null>(null)
/** The URL `lyrics` was fetched from, to skip refetching on a reopen. */
let loadedHref: string | null = null
const loading = ref(false)
/**
 * Increments with every request: an answer only lands if no later request was
 * made in the meantime. Two songs in quick succession must never leave the
 * first one's lyrics under the second one's title.
 */
let generation = 0

async function load(href: string) {
  const mine = ++generation
  loading.value = true
  lyrics.value = null
  loadedHref = null
  try {
    const answer = await api.get<RenderedLyrics>(href)
    if (mine !== generation) return
    lyrics.value = answer
    loadedHref = href
  } catch {
    // A 404 (the key left the core's memory) or a dropped network: the
    // dialog then says there are no lyrics, which is what the listener can
    // act on. Nothing is cached, so a reopen tries again.
    if (mine !== generation) return
    lyrics.value = null
  } finally {
    if (mine === generation) loading.value = false
  }
}

watch(
  [open, () => props.href],
  ([isOpen, href]) => {
    // No href while open: the lyrics vanished with the song (Review Focus 5).
    // Nothing to fetch; the template says so from `href` alone, ahead of
    // whatever was loaded or is still on its way, and the next href's load
    // supersedes both.
    if (!isOpen || href === null) return
    if (href === loadedHref && lyrics.value !== null) return
    void load(href)
  },
  { immediate: true },
)

const lines = computed(() => lyrics.value?.synced ?? [])
/**
 * Timed lines are shown as lines, followed or not: a Stop on a file loses the
 * position, and swapping the list for a paragraph then would throw away the
 * listener's place in it for a text that reads the same.
 */
const listed = computed(() => props.synced && lines.value.length > 0)
/**
 * Follow the song only when it can be followed: timed lines, and a position.
 * A radio no plugin tracks has lines but no position; highlighting the first
 * line forever would claim a precision nobody has.
 */
const following = computed(() => listed.value && props.positionS !== null)
const active = computed(() => (following.value ? activeLine(lines.value, props.positionS) : null))
/** The whole text, for lyrics that are not timed. */
const wholeText = computed(() => {
  const l = lyrics.value
  if (!l) return null
  if (l.plain) return l.plain
  if (l.synced?.length) return l.synced.map((line) => line.text).join('\n')
  return null
})

const scroller = ref<HTMLElement | null>(null)
/** Until when a hand scroll keeps the dialog from following, in `Date.now()` ms. */
let heldUntil = 0
/**
 * Until when a `scroll` event is ours. A smooth `scrollIntoView` fires
 * `scroll` events of its own for the length of its animation; taking them for
 * the listener's would hold the following after every line, i.e. forever.
 * `wheel` and `touchmove` are never ours, so they always count.
 */
let ownScrollUntil = 0
const OWN_SCROLL_MS = 1000

function onHandScroll(event: Event) {
  if (event.type === 'scroll' && Date.now() < ownScrollUntil) return
  heldUntil = Date.now() + MANUAL_SCROLL_HOLD_MS
}

// `open` too: a reopen on the same song finds the same active line, and the
// freshly mounted list starts at its top.
watch([active, open], async ([index, isOpen]) => {
  if (index === null || !isOpen) return
  if (Date.now() < heldUntil) return
  await nextTick()
  // Queried rather than taken from a `v-for` ref array, whose order Vue does
  // not guarantee to follow the list's.
  const element = scroller.value?.querySelector<HTMLElement>('[data-lyrics-active]')
  if (!element) return
  ownScrollUntil = Date.now() + OWN_SCROLL_MS
  // Optional call: jsdom has no `scrollIntoView`, and a browser without it
  // still shows the highlighted line, just without moving to it.
  element.scrollIntoView?.({ block: 'center', behavior: 'smooth' })
})
</script>

<template>
  <Dialog v-model:open="open">
    <DialogTrigger v-if="href" as-child>
      <!-- `size-11`: the 44 px tap target of its neighbours on this row, and
           `relative z-10` for the same reason as theirs — the progress bar
           thumb's hit area overflows onto the row (see PlayerCard.vue). -->
      <Button
        variant="ghost"
        class="relative z-10 size-11 shrink-0 rounded-full text-muted-foreground [&_svg]:size-[18px]"
        :aria-label="t('lyrics_open')"
        :title="t('lyrics_open')"
        data-lyrics-open
      >
        <MicIcon />
      </Button>
    </DialogTrigger>
    <!-- No description: the title says it all, and the lyrics are the body.
         `aria-describedby` undefined says so to reka-ui, which warns
         otherwise. -->
    <DialogContent :aria-describedby="undefined" data-lyrics-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('lyrics_title') }}</DialogTitle>
      </DialogHeader>

      <div
        ref="scroller"
        class="max-h-[60vh] overflow-y-auto text-base leading-relaxed"
        data-lyrics-scroll
        @wheel.passive="onHandScroll"
        @touchmove.passive="onHandScroll"
        @scroll.passive="onHandScroll"
      >
        <p v-if="!href" class="text-muted-foreground" data-lyrics-none>{{ t('lyrics_none') }}</p>
        <p v-else-if="loading" class="text-muted-foreground" role="status" data-lyrics-loading>
          {{ t('lyrics_loading') }}
        </p>
        <ol v-else-if="listed" class="space-y-1" data-lyrics-lines>
          <li
            v-for="(line, i) in lines"
            :key="i"
            :class="i === active ? 'font-semibold text-foreground' : 'text-muted-foreground'"
            :data-lyrics-active="i === active ? '' : undefined"
          >
            <!-- An empty timed line is a pause: a non-breaking space keeps
                 its height, so the rhythm of the song shows. -->
            {{ line.text || ' ' }}
          </li>
        </ol>
        <p v-else-if="wholeText" class="whitespace-pre-line" data-lyrics-text>{{ wholeText }}</p>
        <p v-else class="text-muted-foreground" data-lyrics-none>{{ t('lyrics_none') }}</p>
      </div>

      <p v-if="href && origin" class="text-xs text-muted-foreground" data-lyrics-origin>
        {{ t('lyrics_origin', { par: origin }) }}
      </p>
    </DialogContent>
  </Dialog>
</template>
