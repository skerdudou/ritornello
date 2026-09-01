import { getCurrentInstance, onUnmounted, ref } from 'vue'
import type { PlayerPayload } from '../types'

/**
 * Duration as `m:ss`, or `null` if unknown.
 *
 * No hours: these are music tracks, and a `0:03:34` display would be longer to
 * read for nothing. A negative or absurd duration is treated as unknown rather
 * than rendered as is — it comes from a third party.
 */
export function formatDuration(seconds: number | null | undefined): string | null {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds <= 0) return null
  const m = Math.floor(seconds / 60)
  const s = Math.floor(seconds % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}

/**
 * Same shape as `formatDuration`, but `0` is a legitimate value: a position at
 * the very beginning of a track is written "0:00". A distinct function rather
 * than a relaxation of the other one, whose refusal of zero values avoids
 * displaying "0:00" as the duration of a track whose duration is unknown.
 */
export function formatPosition(seconds: number | null | undefined): string | null {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) return null
  const m = Math.floor(seconds / 60)
  const s = Math.floor(seconds % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}

/**
 * True if the state teaches nothing displayable.
 *
 * The duration alone does not count: "3:34" without title nor artist informs
 * nobody, and would display an empty block.
 */
export function nothingToShow(state: PlayerPayload | null): boolean {
  if (!state) return true
  return !state.artist && !state.title && !state.album
}

/**
 * State of the player, received as a pushed stream from `/api/player`.
 *
 * `EventSource` rather than polling: the core already pushes every change,
 * and the browser reconnects by itself after an outage — no retry logic to
 * write here. The current state arrives as soon as the connection opens, so a
 * tab opened in the middle of a track does not stay empty.
 */
export function usePlayer() {
  const state = ref<PlayerPayload | null>(null)
  let stream: EventSource | null = null

  function ouvre(): void {
    // `EventSource` does not exist everywhere (jsdom under test, old engines):
    // the absence of the current track must not break the rest of the page.
    if (typeof EventSource === 'undefined') {
      console.warn('EventSource unavailable: the current track will not be displayed')
      return
    }
    ferme()
    stream = new EventSource('/api/player')
    stream.onmessage = (e: MessageEvent) => {
      try {
        state.value = JSON.parse(e.data as string) as PlayerPayload
      } catch {
        // Unreadable frame: keep the previous display rather than emptying it.
      }
    }
    // No error handling: `EventSource` recovers on its own, and closing here
    // would deprive the page of any recovery after a restart of the core
    // (most common case: `systemctl restart ritornello`).
  }

  function ferme(): void {
    stream?.close()
    stream = null
  }

  // Usable outside a component (tests): `onUnmounted` without a current
  // instance would trigger a Vue warning.
  if (getCurrentInstance()) onUnmounted(ferme)

  return { state, ouvre, ferme }
}
