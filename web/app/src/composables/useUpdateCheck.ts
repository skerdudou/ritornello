import { api } from '@ritornello/ui'
import { onScopeDispose, ref, watch } from 'vue'
import type { UpdatePayload } from '../types'

/**
 * A check younger than this is trusted as it stands: opening a dialog does
 * not ask the device to look again. One hour is the operator's own decision
 * (follow-up B); the daily scheduled check is what keeps a device honest
 * beyond it, and the dialogs are the place where "nothing offered" must
 * never mean "nobody looked".
 */
export const CHECK_FRESH_S = 3600

const POLL_MS = 2000
/** Ticks of `POLL_MS` before the wait gives up: 20 s. A check that fails the
 * same way twice publishes nothing this poll can tell apart from "still
 * running", so the ceiling is what ends the wait for it, exactly like
 * `pollLanguageWhileBusy` in `ConfigView`. */
const MAX_TICKS = 10

export type CheckPhase = 'idle' | 'checking' | 'timeout' | 'queue_full' | 'error'

/** What the composable reads of `GET /api/update`. */
export interface CheckState {
  outcome: UpdatePayload['outcome']
  lastCheckUnixS: number | null
  busy: string | null
}

export interface UpdateCheckOptions {
  open: () => boolean
  /** The page's current view of the update state, read when a dialog opens. */
  state: () => CheckState
  /** Called once the wait is over, so the page reloads what it shows. */
  onSettled: () => void
  nowS?: () => number
}

/**
 * The check both "Add a plugin" and "Add a language" run when they open, so
 * that neither ever shows an empty list because nobody has looked yet.
 *
 * - A recent (under one hour), successful check is left alone.
 * - A job already running is only waited for: a second `POST` would queue a
 *   second check behind the first (the queue holds four).
 * - Otherwise one `POST /api/update/check`, answered 202 on enqueue only,
 *   then `GET /api/update` every 2 s until the check has visibly landed:
 *   `last_check_unix_s` or the outcome moved, or a busy state came and went.
 *   `busy` alone is never the signal: right after the 202 the worker may not
 *   have taken its write lock yet, and a poll that stopped on `busy: null`
 *   would stop before the check began.
 * - A 429 (queue full) is its own phase, not a failure of the check.
 */
export function useUpdateCheck(options: UpdateCheckOptions) {
  const phase = ref<CheckPhase>('idle')
  /** Why the request itself failed (`phase === 'error'`). */
  const error = ref<string | null>(null)
  const nowS = options.nowS ?? (() => Date.now() / 1000)

  let generation = 0
  let timer: ReturnType<typeof setInterval> | null = null

  function stopPoll() {
    if (timer !== null) {
      clearInterval(timer)
      timer = null
    }
  }

  function fresh(s: CheckState): boolean {
    return (
      s.outcome.kind !== 'failed'
      && s.lastCheckUnixS !== null
      // A negative age is a Pi whose clock is ahead of the browser's: that
      // must never read as "fresh for a long time".
      && nowS() - s.lastCheckUnixS >= 0
      && nowS() - s.lastCheckUnixS < CHECK_FRESH_S
    )
  }

  /**
   * The state to decide on: read from the device right now, never the page's
   * copy. The page's `update` is refreshed only by its own polls, so a dialog
   * closed and reopened while its check was still running, or opened just
   * after the update card's Check, would read `busy: null` and an old
   * timestamp and queue a second check. The page's copy is only the fallback
   * when the device cannot be read.
   */
  async function currentState(): Promise<CheckState> {
    try {
      const p = await api.get<UpdatePayload>('/api/update')
      return { outcome: p.outcome, lastCheckUnixS: p.last_check_unix_s, busy: p.busy }
    } catch {
      return options.state()
    }
  }

  function wait(gen: number, s: CheckState, seenBusy: boolean) {
    const outcomeAtStart = JSON.stringify(s.outcome)
    let ticks = 0
    stopPoll()
    timer = setInterval(async () => {
      ticks += 1
      let latest: UpdatePayload | null = null
      try {
        latest = await api.get<UpdatePayload>('/api/update')
      } catch {
        // Unreachable this tick: the ceiling decides.
      }
      if (gen !== generation) return
      let done = false
      if (latest) {
        done =
          latest.last_check_unix_s !== s.lastCheckUnixS
          || JSON.stringify(latest.outcome) !== outcomeAtStart
          || (seenBusy && !latest.busy)
        seenBusy = seenBusy || !!latest.busy
      }
      if (done) {
        stopPoll()
        phase.value = 'idle'
        options.onSettled()
      } else if (ticks >= MAX_TICKS) {
        if (latest?.outcome.kind === 'failed') {
          // Failed the same way as before: nothing can tell it from a check
          // still running, and the failure is what there is to show.
          stopPoll()
          phase.value = 'idle'
          options.onSettled()
        } else {
          // Still nothing. Say so, with Retry, and keep looking while the
          // dialog is open: a check that lands late refreshes the rows.
          phase.value = 'timeout'
        }
      }
    }, POLL_MS)
  }

  async function start() {
    const gen = ++generation
    stopPoll()
    error.value = null
    phase.value = 'checking'
    const s = await currentState()
    if (gen !== generation) return
    if (s.busy) {
      wait(gen, s, true)
      return
    }
    if (fresh(s)) {
      phase.value = 'idle'
      // The page may hold an older view than the device just answered with.
      options.onSettled()
      return
    }
    // `api.post` never rejects: a failure is the returned string.
    const err = await api.post('/api/update/check', undefined)
    if (gen !== generation) return
    if (err) {
      error.value = err
      phase.value = /\b429\b/.test(err) ? 'queue_full' : 'error'
      return
    }
    wait(gen, s, false)
  }

  watch(
    options.open,
    (open) => {
      if (open) {
        void start()
      } else {
        generation += 1
        stopPoll()
        phase.value = 'idle'
      }
    },
    { immediate: true },
  )
  onScopeDispose(() => {
    generation += 1
    stopPoll()
  })

  return { phase, error, retry: () => start() }
}

/**
 * The failure a dialog shows with its Retry button, or `null`: the refusal of
 * the request itself, or, once the wait is over, the `failed` outcome the
 * check left. Never while a check is running or the queue was full: those
 * have their own sentence.
 */
export function checkFailure(
  phase: CheckPhase,
  error: string | null,
  outcome: UpdatePayload['outcome'],
): string | null {
  if (phase === 'error') return error
  if (phase === 'idle' && outcome.kind === 'failed') return outcome.detail
  return null
}

/**
 * Whether the last check actually looked, and can therefore be trusted to
 * mean "nothing to add" when a dialog's list comes back empty.
 *
 * `never_checked`, `no_release` and `only_prereleases` always rebuild every
 * component against an empty published list server-side (`component_offers`
 * called with `&[]`, see `update/mod.rs`'s `check`), so every row resolves to
 * `unknown` and the list is empty regardless of what the appliance actually
 * has — an empty list there is silence, not completeness.
 *
 * `failed` is not always that kind of silence (N3). It is published both for
 * a failed check (`publish_failure`, which leaves `components` exactly as an
 * earlier successful check left them) and for a *refused install* that
 * followed a successful check (`install_report`/`conclude_install`, which
 * re-checks before installing) — in both cases the rows on screen, and the
 * `lastCheckUnixS`, are still the real ones from that earlier success. Only a
 * `failed` on a device that has genuinely **never** succeeded — `null` here —
 * is the silent kind. `ok` and `installed` (the transient report right after
 * a successful install, still built from a real release) are never silent
 * either way.
 */
export function hasUsableCheck(
  outcome: UpdatePayload['outcome'],
  lastCheckUnixS: number | null,
): boolean {
  return (
    outcome.kind === 'ok'
    || outcome.kind === 'installed'
    || (outcome.kind === 'failed' && lastCheckUnixS !== null)
  )
}
