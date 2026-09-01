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
 */
export function usePresets() {
  const names = ref<Map<string, Map<number, string>>>(new Map())

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
    names.value = new Map(
      load.sources.map((s) => [s.name, new Map((s.presets ?? []).map((p) => [p.index, p.name]))]),
    )
  }

  function nameOf(source: string, n: number): string | null {
    return names.value.get(source)?.get(n) ?? null
  }

  return { reload, nameOf }
}
