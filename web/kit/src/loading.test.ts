import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, ref, watch, type Ref } from 'vue'
import { SKELETON_DELAY_MS, SKELETON_MIN_VISIBLE_MS, useSkeleton } from './loading'

/**
 * Records every value the skeleton flag takes, so that a test can assert it
 * was **never** true rather than merely "not true at the moment I looked".
 * A sample taken at one instant cannot see a flash that opened and closed
 * between two `advanceTimersByTime`; the trace can.
 */
function trace(source: Readonly<Ref<boolean>>): boolean[] {
  const seen = [source.value]
  watch(source, (v) => seen.push(v))
  return seen
}

describe('useSkeleton', () => {
  let scope: ReturnType<typeof effectScope>

  beforeEach(() => {
    vi.useFakeTimers()
    scope = effectScope()
  })

  afterEach(() => {
    scope.stop()
    vi.useRealTimers()
  })

  /**
   * Advances the clock the way a browser does: pending reactivity is flushed
   * **first**, then time passes.
   *
   * Draining afterwards only would model an event loop that fires a 200 ms
   * timer before running a microtask queued at t=0 — which no browser does,
   * and which alone made the first test see a skeleton the real page never
   * shows.
   */
  async function tick(ms: number): Promise<void> {
    await nextTick()
    vi.advanceTimersByTime(ms)
    await nextTick()
  }

  it('never shows the skeleton when the wait ends before the delay', async () => {
    const pending = ref(true)
    const skeleton = scope.run(() => useSkeleton(pending))!
    const seen = trace(skeleton)
    await nextTick()

    await tick(SKELETON_DELAY_MS - 1)
    pending.value = false
    await tick(10_000)

    expect(seen).toEqual([false])
  })

  it('shows the skeleton once the wait outlasts the delay', async () => {
    const pending = ref(true)
    const skeleton = scope.run(() => useSkeleton(pending))!
    await nextTick()

    await tick(SKELETON_DELAY_MS - 1)
    expect(skeleton.value).toBe(false)

    await tick(1)
    expect(skeleton.value).toBe(true)
  })

  it('holds the skeleton for a minimum once it is on screen', async () => {
    const pending = ref(true)
    const skeleton = scope.run(() => useSkeleton(pending))!
    await nextTick()

    await tick(SKELETON_DELAY_MS)
    expect(skeleton.value).toBe(true)

    // The data lands 10 ms after the skeleton appeared: without the floor,
    // this is the 10 ms grey flash that is worse than the jump it replaced.
    await tick(10)
    pending.value = false
    await tick(SKELETON_MIN_VISIBLE_MS - 11)
    expect(skeleton.value).toBe(true)

    await tick(1)
    expect(skeleton.value).toBe(false)
  })

  it('drops a pending delay when the wait ends, so a later wait starts from zero', async () => {
    const pending = ref(true)
    const skeleton = scope.run(() => useSkeleton(pending))!
    const seen = trace(skeleton)
    await nextTick()

    await tick(SKELETON_DELAY_MS - 1)
    pending.value = false
    await tick(1)
    // A second wait begins here. Were the first delay still armed, the
    // skeleton would appear at once instead of waiting its own delay.
    pending.value = true
    await tick(SKELETON_DELAY_MS - 1)

    expect(seen).toEqual([false])
    await tick(1)
    expect(skeleton.value).toBe(true)
  })

  it('arms no timer once its scope is disposed', async () => {
    const pending = ref(true)
    const skeleton = scope.run(() => useSkeleton(pending))!
    await nextTick()

    scope.stop()
    await tick(10_000)

    expect(skeleton.value).toBe(false)
    expect(vi.getTimerCount()).toBe(0)
  })
})
