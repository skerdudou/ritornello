import type { ComponentOffer } from '../types'

/**
 * What the update dialog ticks by itself: what is out of step and nothing
 * else — `update_available`, never third-party (its own repository decides,
 * not this release), and never a component already known to need a manual
 * step (checking it again would only repeat the same refusal).
 *
 * "Third-party" is read from `third_party_repo` (ruling P6), not from the
 * kind alone: a stranger's **language pack** has the kind `language_pack`
 * and carries its source there, and used to be pre-ticked like one of ours.
 * The kind is kept as well, so a third-party plugin row that names no
 * repository fails closed too.
 *
 * Shared with the update card, which names what a waiting major update would
 * leave refused if the operator accepted these defaults.
 */
export function defaultSelection(components: ComponentOffer[]): Set<string> {
  return new Set(
    components
      .filter((c) => c.kind !== 'third_party' && !c.third_party_repo)
      .filter((c) => c.availability === 'update_available')
      .filter((c) => c.installable !== false)
      .map((c) => c.name),
  )
}

/** Does the core's own row offer a core that breaks the wire. */
export function coreBreaks(components: ComponentOffer[]): boolean {
  return components.some((c) => c.kind === 'core' && c.breaking === true && c.offered !== null)
}

/**
 * The plugins the **offered** core will refuse once installed, given what is
 * installed with it (`selected`). Read from the core's verdicts only, never
 * deduced here:
 *
 * - a plugin installed with it: its offered version against that core
 *   (`with_core`);
 * - a plugin with an update left behind: refused when its new version needs
 *   the new core (`with_running_core` refused — the core's own definition of
 *   a plugin that depends on it), since what stays installed was built for
 *   the core being replaced;
 * - a plugin the release does not update (a third party's, typically): its
 *   live announcement against that core (`with_core`).
 *
 * A row whose contracts are unpublished carries no verdict and is not listed:
 * the device cannot tell, and says so on that row instead.
 */
export function refusedByNewCore(components: ComponentOffer[], selected: Set<string>): string[] {
  return components
    .filter((c) => c.kind === 'plugin' || c.kind === 'third_party')
    .filter((c) => {
      if (selected.has(c.name)) return c.with_core?.fit === 'refused'
      if (c.with_running_core) return c.with_running_core.fit === 'refused'
      return c.with_core?.fit === 'refused'
    })
    .map((c) => c.name)
}
