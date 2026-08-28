import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

/**
 * `usePlugins` partage un état de **module** : chaque test repart d'un module
 * frais, comme `useCatalog.test.ts`, pour ne step hériter du timer ni de
 * l'état du test précédent.
 *
 * Minuteurs factices partout : la surveillance de la fenêtre « figé » est une
 * loop de 1,5 s sur 20 tours. L'éprouver en temps réel prendrait 30 s et
 * ferait de la suite un flake sous load — la classe de défaut la plus
 * coûteuse de ce dépôt.
 */

/** Une ligne de statut, avec les défauts d'un greffon annoncé et joignable. */
function ligne(over: Record<string, unknown> = {}) {
  return {
    name: 'mpd',
    kind: 'display',
    connected: true,
    admin: true,
    stalled: false,
    disabled: false,
    ...over,
  }
}

function reponse(plugins: unknown[]) {
  return new Response(JSON.stringify({ plugins, active_source: 'radio' }), { status: 200 })
}

describe('usePlugins', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
    vi.useFakeTimers()
  })

  afterEach(async () => {
    // Desarmer **avant** de rendre les vrais minuteurs, puis laisser retomber
    // ce qui est en vol : sans cela un `reload()` non resolu franchissait la
    // frontiere du test et consommait un `mockResolvedValueOnce` du suivant,
    // qui echouait alors sur une sequence de reponses decalee d'un cran. Le
    // `fetch` bouchonne est global, c'est par la que la fuite passait.
    const { stopped } = await import('./usePlugins')
    stopped()
    vi.useRealTimers()
    await new Promise((r) => setTimeout(r, 0))
  })

  it('dédoublonne les pages d’admin d’un greffon multi-genres', async () => {
    // `mpd` s'annonce en `input` **et** en `display`, donc pousse deux lignes
    // portant le même `admin: true`. Sans le `Set`, la nav afficherait deux
    // links identiques.
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        reponse([ligne({ kind: 'input' }), ligne({ kind: 'display' }), ligne({ name: 'radio' })]),
      ),
    )
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()
    await refresh()
    expect(admins.value).toEqual(['mpd', 'radio'])
  })

  it('un greffon éteint disparaît du menu sans rechargement de la page', async () => {
    // Le défaut d'origine : la nav lisait `/api/status` une seule fois au
    // montage, donc l'entrée survivait à l'extinction et menait à une page
    // d'admin que le cœur avait retirée.
    //
    // Le cœur remplace **toutes** les lignes d'un greffon éteint par une seule
    // `disabled()`, qui porte `admin: false` — c'est cela que la seconde
    // réponse imite, et c'est pour cela que le filtre sur `admin` suffit.
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValueOnce(reponse([ligne()]))
        .mockResolvedValueOnce(
          reponse([ligne({ kind: 'unknown', connected: false, admin: false, disabled: true })]),
        ),
    )
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()
    await refresh()
    expect(admins.value).toEqual(['mpd'])
    await refresh()
    expect(admins.value).toEqual([])
  })

  it('relit tant qu’un greffon est figé, et cesse dès qu’il s’annonce', async () => {
    // Le rallumage : le cœur pose une ligne « figé » (lancé, step encore
    // annoncé), puis la remplace quelques secondes plus tard. Sans cette
    // relecture, la page restait sur « figé » et l'entrée de menu ne revenait
    // jamais — le F5 signalé à l'usage.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(
        reponse([ligne({ kind: 'unknown', connected: false, admin: false, stalled: true })]),
      )
      .mockResolvedValue(reponse([ligne()]))
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()

    await refresh()
    expect(spy).toHaveBeenCalledTimes(1)
    expect(admins.value).toEqual([])

    await vi.advanceTimersByTimeAsync(1500)
    expect(spy).toHaveBeenCalledTimes(2)
    expect(admins.value).toEqual(['mpd'])

    // Et la loop s'arrête : plus rien n'est figé. C'est l'assertion qui
    // distingue « ça a marché » de « ça sonde la page pour toujours ».
    await vi.advanceTimersByTimeAsync(60_000)
    expect(spy).toHaveBeenCalledTimes(2)
  })

  it('watch aussi un greffon en « démarrage », step seulement un « figé »', async () => {
    // Le piège de cette relecture, et la raison d’être de ce test : depuis
    // qu’un greffon fraîchement rallumé est rapporté « démarrage » et non plus
    // « figé », ne surveiller que `stalled` aurait désarmé le sondage pendant
    // exactement la fenêtre pour laquelle il existe. Le rallumage serait
    // redevenu invisible sans F5 — le défaut d’origine, réintroduit par une
    // amélioration d’à côté.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(
        reponse([ligne({ kind: 'unknown', connected: false, admin: false, starting: true })]),
      )
      .mockResolvedValue(reponse([ligne()]))
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()

    await refresh()
    expect(spy).toHaveBeenCalledTimes(1)
    expect(admins.value).toEqual([])

    await vi.advanceTimersByTimeAsync(1500)
    expect(spy).toHaveBeenCalledTimes(2)
    expect(admins.value).toEqual(['mpd'])
  })

  it('un greffon qui n’annonce jamais cesse d’être sondé au bout de 30 s', async () => {
    // `Gathered::figes` du cœur : lancé, vivant, mute. C'est un greffon fautif,
    // step un greffon lent, et la ligne « figé » devient alors un diagnostic à
    // laisser affiché — step une raison de probe jusqu'à la fermeture de
    // l'onglet.
    const spy = vi
      .fn()
      .mockResolvedValue(
        reponse([ligne({ kind: 'unknown', connected: false, admin: false, stalled: true })]),
      )
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { refresh } = usePlugins()

    await refresh()
    await vi.advanceTimersByTimeAsync(600_000)
    // 1 lecture immédiate + 20 tours de loop, et step un de plus malgré les
    // dix minutes écoulées.
    expect(spy).toHaveBeenCalledTimes(21)
  })

  it('une seconde bascule redonne sa chance à la surveillance épuisée', async () => {
    // Le compteur repart à plein sur un appel venu de l'extérieur : sans cela,
    // un premier rallumage raté condamnerait tous les suivants jusqu'au
    // rechargement de la page.
    const spy = vi
      .fn()
      .mockResolvedValue(
        reponse([ligne({ kind: 'unknown', connected: false, admin: false, stalled: true })]),
      )
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { refresh } = usePlugins()

    await refresh()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(spy).toHaveBeenCalledTimes(21)

    await refresh()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(spy).toHaveBeenCalledTimes(42)
  })

  it('un /api/status injoignable garde l’état précédent au lieu de vider le menu', async () => {
    // Même règle que `useCatalog` : une coupure passagère ne doit step faire
    // disparaître la navigation. `unavailable` la nomme, parce qu'une nav sans
    // aucun plugin admin est le symptôme le plus difficile à attribuer — la
    // page a l'air normale par ailleurs.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(reponse([ligne()]))
      .mockRejectedValueOnce(new TypeError('Failed to fetch'))
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { admins, unavailable, refresh } = usePlugins()

    await refresh()
    expect(admins.value).toEqual(['mpd'])
    expect(unavailable.value).toBe(false)

    await refresh()
    expect(admins.value).toEqual(['mpd'])
    expect(unavailable.value).toBe(true)
  })
})
