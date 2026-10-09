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

/**
 * Will the core it meets refuse this plugin **if nothing is placed for it** —
 * the binary on the device left as it is.
 *
 * Read from `installed_with_core`, the core's own verdict on what that binary
 * announced: exact, whether or not the row has an update. Inferring it from
 * the offered version was wrong both ways — a dependent whose new version
 * *adds* the moved contract keeps a binary the new core accepts, and a third
 * party still built for the old major is refused while its offered version
 * says nothing about it.
 *
 * Only a plugin that announced nothing (disabled, not started) lacks that
 * verdict; then the offered version's verdicts are all there is, and either
 * one refused counts.
 */
export function refusedIfLeftAlone(c: ComponentOffer): boolean {
  if (c.installed_with_core) return c.installed_with_core.fit === 'refused'
  return c.with_running_core?.fit === 'refused' || c.with_core?.fit === 'refused'
}

/**
 * The plugins the **offered** core will refuse once installed, given what is
 * installed with it (`selected`). Read from the core's verdicts only:
 *
 * - a plugin installed with it: its offered version against that core
 *   (`with_core`);
 * - any other plugin on the device: its binary left as it is
 *   (`refusedIfLeftAlone`).
 *
 * A component the device does not have (no binary) has nothing to refuse.
 */
export function refusedByNewCore(components: ComponentOffer[], selected: Set<string>): string[] {
  return components
    .filter((c) => c.kind === 'plugin' || c.kind === 'third_party')
    .filter((c) => {
      if (selected.has(c.name)) return c.with_core?.fit === 'refused'
      return c.binary_present && refusedIfLeftAlone(c)
    })
    .map((c) => c.name)
}
