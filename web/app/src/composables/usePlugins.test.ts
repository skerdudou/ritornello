import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

/**
 * `usePlugins` shares **module** state: each test starts over from a fresh
 * module, like `useCatalog.test.ts`, so as not to inherit the timer nor the
 * state of the previous test.
 *
 * Fake timers everywhere: the watch of the "stalled" window is a 1.5 s loop
 * over 20 turns. Testing it in real time would take 30 s and would make the
 * suite a flake under load — the most expensive class of defect in this
 * repository.
 */

/** A status row, with the defaults of an announced and reachable plugin. */
function row(over: Record<string, unknown> = {}) {
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

function response(plugins: unknown[]) {
  return new Response(JSON.stringify({ plugins, active_source: 'radio' }), { status: 200 })
}

describe('usePlugins', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
    vi.useFakeTimers()
  })

  afterEach(async () => {
    // Disarm **before** restoring the real timers, then let what is in flight
    // settle: without this an unresolved `reload()` crossed the boundary of
    // the test and consumed a `mockResolvedValueOnce` of the next one, which
    // then failed on a sequence of responses shifted by one. The stubbed
    // `fetch` is global, that is where the leak went through.
    const { stopped } = await import('./usePlugins')
    stopped()
    vi.useRealTimers()
    await new Promise((r) => setTimeout(r, 0))
  })

  it('deduplicates the admin pages of a multi-kind plugin', async () => {
    // `mpd` announces itself as `input` **and** as `display`, so pushes two
    // rows carrying the same `admin: true`. Without the `Set`, the nav would
    // display two identical links.
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        response([row({ kind: 'input' }), row({ kind: 'display' }), row({ name: 'radio' })]),
      ),
    )
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()
    await refresh()
    expect(admins.value).toEqual(['mpd', 'radio'])
  })

  it('a disabled plugin disappears from the menu without reloading the page', async () => {
    // The original defect: the nav read `/api/status` once only at mount, so
    // the entry survived the disabling and led to an admin page the core had
    // removed.
    //
    // The core replaces **all** the rows of a disabled plugin by a single
    // `disabled()`, which carries `admin: false` — that is what the second
    // response mimics, and that is why the filter on `admin` is enough.
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValueOnce(response([row()]))
        .mockResolvedValueOnce(
          response([row({ kind: 'unknown', connected: false, admin: false, disabled: true })]),
        ),
    )
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()
    await refresh()
    expect(admins.value).toEqual(['mpd'])
    await refresh()
    expect(admins.value).toEqual([])
  })

  it('re-reads as long as a plugin is stalled, and stops as soon as it announces itself', async () => {
    // Re-enabling: the core sets a "stalled" row (launched, not announced
    // yet), then replaces it a few seconds later. Without this re-read, the
    // page stayed on "stalled" and the menu entry never came back — the F5
    // reported in use.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(
        response([row({ kind: 'unknown', connected: false, admin: false, stalled: true })]),
      )
      .mockResolvedValue(response([row()]))
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { admins, refresh } = usePlugins()

    await refresh()
    expect(spy).toHaveBeenCalledTimes(1)
    expect(admins.value).toEqual([])

    await vi.advanceTimersByTimeAsync(1500)
    expect(spy).toHaveBeenCalledTimes(2)
    expect(admins.value).toEqual(['mpd'])

    // And the loop stops: nothing is stalled any more. This is the assertion
    // that distinguishes "it worked" from "it probes the page forever".
    await vi.advanceTimersByTimeAsync(60_000)
    expect(spy).toHaveBeenCalledTimes(2)
  })

  it('also watches a "starting" plugin, not only a "stalled" one', async () => {
    // The trap of this re-read, and the reason this test exists: since a
    // freshly re-enabled plugin is reported "starting" and no longer
    // "stalled", watching only `stalled` would have disarmed the probing
    // during exactly the window it exists for. Re-enabling would have become
    // invisible without F5 again — the original defect, reintroduced by a
    // neighbouring improvement.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(
        response([row({ kind: 'unknown', connected: false, admin: false, starting: true })]),
      )
      .mockResolvedValue(response([row()]))
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

  it('a plugin that never announces itself stops being probed after 30 s', async () => {
    // `Gathered::figes` of the core: launched, alive, silent. This is a faulty
    // plugin, not a slow one, and the "stalled" row then becomes a diagnosis
    // to leave displayed — not a reason to probe until the tab is closed.
    const spy = vi
      .fn()
      .mockResolvedValue(
        response([row({ kind: 'unknown', connected: false, admin: false, stalled: true })]),
      )
    vi.stubGlobal('fetch', spy)
    const { usePlugins } = await import('./usePlugins')
    const { refresh } = usePlugins()

    await refresh()
    await vi.advanceTimersByTimeAsync(600_000)
    // 1 immediate read + 20 turns of the loop, and not one more despite the
    // ten minutes elapsed.
    expect(spy).toHaveBeenCalledTimes(21)
  })

  it('a second toggle gives the exhausted watch another chance', async () => {
    // The counter starts over at full on a call coming from outside: without
    // this, a first failed re-enabling would doom all the following ones
    // until the page is reloaded.
    const spy = vi
      .fn()
      .mockResolvedValue(
        response([row({ kind: 'unknown', connected: false, admin: false, stalled: true })]),
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

  it('an unreachable /api/status keeps the previous state instead of emptying the menu', async () => {
    // Same rule as `useCatalog`: a transient outage must not make the
    // navigation disappear. `unavailable` names it, because a nav without any
    // admin plugin is the hardest symptom to attribute — the page looks
    // normal otherwise.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(response([row()]))
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
