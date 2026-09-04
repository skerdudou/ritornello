import { onScopeDispose, ref, toValue, watch, type MaybeRefOrGetter, type Ref } from 'vue'

/**
 * How long a wait must last before it is worth telling the user about.
 *
 * Below this, the page has nothing to gain from a placeholder: the content
 * arrives before the eye has settled, and a skeleton shown for 120 ms reads as
 * a flicker — the very agitation it was meant to remove. Kept under the ~250 ms
 * at which a delay starts being perceived as one, so that a wait long enough to
 * notice is never left unexplained.
 */
export const SKELETON_DELAY_MS = 200

/**
 * Once a skeleton is on screen, how long it stays there whatever happens.
 *
 * Without this floor the two thresholds fight each other: data that lands
 * 10 ms after the delay elapsed would paint a grey flash and wipe it, which is
 * strictly worse than the layout jump the skeleton replaced. The floor buys
 * the appearance a duration of its own.
 */
export const SKELETON_MIN_VISIBLE_MS = 300

export interface SkeletonOptions {
  /** Overrides {@link SKELETON_DELAY_MS}. */
  delay?: number
  /** Overrides {@link SKELETON_MIN_VISIBLE_MS}. */
  minVisible?: number
}

/**
 * Tells a view whether to paint a loading placeholder, on the rhythm above.
 *
 * The flag alone drives three states, which is why it is a single ref and not
 * a machine: while it is false and the wait is still on, the view renders
 * **nothing** — the frame stays empty rather than showing a half-built page.
 * Templates therefore read:
 *
 * ```html
 * <MySkeleton v-if="skeleton" />
 * <RealContent v-else-if="data" />
 * ```
 *
 * The empty first branch is deliberate: it is what a fast load looks like, and
 * it is why the page appears in one piece instead of assembling itself under
 * the reader's eyes.
 */
export function useSkeleton(
  pending: MaybeRefOrGetter<boolean>,
  options: SkeletonOptions = {},
): Readonly<Ref<boolean>> {
  const delay = options.delay ?? SKELETON_DELAY_MS
  const minVisible = options.minVisible ?? SKELETON_MIN_VISIBLE_MS

  const visible = ref(false)
  let timer: ReturnType<typeof setTimeout> | undefined
  // Wall-clock instant at which the skeleton appeared, and the only way to
  // know how much of its minimum it has already served: the wait may end at
  // any point, so the remaining hold has to be computed, not scheduled up
  // front.
  let shownAt = 0

  function disarm(): void {
    if (timer === undefined) return
    clearTimeout(timer)
    timer = undefined
  }

  watch(
    () => toValue(pending),
    (isPending) => {
      // Every transition starts by disarming: a delay left armed across the
      // end of a wait would fire under the *next* one and show its skeleton
      // instantly, robbing that wait of its own grace period.
      disarm()

      if (isPending) {
        // A wait that starts while the skeleton is still serving its minimum
        // simply keeps it: hiding and re-showing it would be the flicker the
        // floor exists to prevent.
        if (visible.value) return
        timer = setTimeout(() => {
          visible.value = true
          shownAt = Date.now()
        }, delay)
        return
      }

      if (!visible.value) return
      const remaining = minVisible - (Date.now() - shownAt)
      if (remaining <= 0) {
        visible.value = false
        return
      }
      timer = setTimeout(() => {
        visible.value = false
      }, remaining)
    },
    { immediate: true },
  )

  // `true` = stay silent outside a scope: a plugin UI is free to call this
  // from a place Vue does not own, and a console warning there would be noise,
  // not a defect — the timer is short-lived either way.
  onScopeDispose(disarm, true)

  return visible
}
