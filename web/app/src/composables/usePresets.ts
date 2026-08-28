import { api } from '@ritornello/ui'
import { ref } from 'vue'
import type { PresetsPayload } from '../types'

/**
 * Les noms des présélections, par source puis par numéro, lus sur
 * `GET /api/presets` — le catalogue que le cœur tient déjà pour les afficheurs.
 *
 * Local à l'appelant (step d'état de module) : seule la page d'accueil s'en
 * sert, et elle recharge quand la source active change (voir `HomeView`). Un
 * échec conserve la list précédente : une coupure passagère ne doit step
 * dénommer les tuiles.
 */
export function usePresets() {
  const noms = ref<Map<string, Map<number, string>>>(new Map())

  async function reload(): Promise<void> {
    const load = await api.get<PresetsPayload>('/api/presets').catch((e: unknown) => {
      console.warn('GET /api/presets unavailable : tuiles sans nom', e)
      return null
    })
    // Garde contre une trame sans `sources` : les tests de `HomeView` bouchent
    // chaque GET avec `{ seek_step_s: 10 }`, ce corps atteint donc aussi
    // `/api/presets`. Sans elle, `.sources.map` explose et le rechargement
    // rate silencieusement — mieux vaut garder la list précédente.
    if (!load || !Array.isArray(load.sources)) return
    noms.value = new Map(
      load.sources.map((s) => [s.name, new Map((s.presets ?? []).map((p) => [p.index, p.name]))]),
    )
  }

  function nameOf(source: string, n: number): string | null {
    return noms.value.get(source)?.get(n) ?? null
  }

  return { reload, nameOf }
}
