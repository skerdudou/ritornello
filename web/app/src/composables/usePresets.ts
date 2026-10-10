import { api } from '@ritornello/ui'
import { ref } from 'vue'
import type { PresetsPayload } from '../types'

/**
 * The names of the presets, by source then by number, read from
 * `GET /api/presets` — the catalog the core already keeps for the displays.
 *
 * Local to the caller (no module state): only the home page uses it, and it
 * reloads when the active source changes (see `HomeView`). A failure keeps
 * the previous list: a transient outage must not strip the tiles of their
 * names.
 *
 * The same catalog also gives the **list** of sources, in the order of the
 * `SourceCycle` key (`sources_catalog` on the core side): the home page draws
 * one key per source from it in the Player card's header when they fit. It is
 * only as fresh as the last reload — a plugin switched on while the page is open shows at the next
 * source change, like its preset names.
 */
export function usePresets() {
  const names = ref<Map<string, Map<number, string>>>(new Map())
  const sources = ref<string[]>([])
  // The icon each source announced, by source name; a source that announced
  // none has no entry (the page then draws its initial).
  const icons = ref<Map<string, string>>(new Map())
  // Whether the list has been read at least once: an empty `sources` alone
  // cannot tell a device with no source from a catalog that never arrived,
  // and the page keeps its cycle key for the second.
  const listRead = ref(false)

  async function reload(): Promise<void> {
    const load = await api.get<PresetsPayload>('/api/presets').catch((e: unknown) => {
      console.warn('GET /api/presets unavailable: tiles without names', e)
      return null
    })
    // Guard against a frame without `sources`: the `HomeView` tests stub every
    // GET with `{ seek_step_s: 10 }`, so that body also reaches
    // `/api/presets`. Without it, `.sources.map` blows up and the reload fails
    // silently — better to keep the previous list.
    if (!load || !Array.isArray(load.sources)) return
    sources.value = load.sources.map((s) => s.name)
    listRead.value = true
    icons.value = new Map(load.sources.flatMap((s): [string, string][] => (s.icon ? [[s.name, s.icon]] : [])))
    names.value = new Map(
      load.sources.map((s) => [s.name, new Map((s.presets ?? []).map((p) => [p.index, p.name]))]),
    )
  }

  function nameOf(source: string, n: number): string | null {
    return names.value.get(source)?.get(n) ?? null
  }

  // The highest preset index the catalog lists for `source`, 0 when it lists
  // none: the grid's count in standby, where the core has forgotten the
  // count the source announced but the catalog still holds its presets.
  function listedCount(source: string): number {
    const m = names.value.get(source)
    return m && m.size ? Math.max(...m.keys()) : 0
  }

  return { reload, nameOf, listedCount, sources, icons, listRead }
}
