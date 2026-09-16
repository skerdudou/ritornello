import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

/**
 * `useMetrics` holds **module** state — a single probe for the whole SPA — so
 * each test starts from a fresh module rather than inheriting the timer, the
 * history and the pause flag of the previous one. Same discipline as
 * `usePlugins.test.ts`, and for the same reason: the leak is silent, it only
 * shows up as a neighbouring test failing on a response sequence shifted by
 * one.
 *
 * Fake timers throughout: the probe runs on a 5 s interval by default, and
 * waiting for it in real time would make this suite the slowest of the
 * repository and a flake under load.
 */

/** Minimal `/api/system` payload: enough for a probe to succeed. The metrics
 *  themselves are not what these tests are about — the connection state is. */
function payload(over: Record<string, unknown> = {}) {
  return { service_uptime_s: 12, ...over }
}

function ok(body: Record<string, unknown> = payload()) {
  return new Response(JSON.stringify(body), { status: 200 })
}

/** A successful `/api/system` response carrying a given version (and,
 *  optionally, a `session` — kept as a wire-shape stress case, not as
 *  something `staleUi` reads any more). Built on `payload`/`ok` rather than a
 *  parallel harness. */
function systemResponse(over: Record<string, unknown> = {}) {
  return ok(payload(over))
}

/** Lets the probe's `await` chain settle. The probe is asynchronous even when
 *  `fetch` resolves at once, so a bare `advanceTimersByTime` returns before
 *  `unavailable` has been written. */
async function settle() {
  await vi.advanceTimersByTimeAsync(0)
}

describe('useMetrics connection state', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
    vi.useFakeTimers()
  })

  afterEach(async () => {
    const { resetMetrics } = await import('./useMetrics')
    resetMetrics()
    vi.useRealTimers()
  })

  it('reports `unknown` until the first probe answers', async () => {
    // A pending `fetch`: the probe is in flight, nothing has been measured
    // yet. Reporting "online" here would claim a fact nobody has checked;
    // reporting "offline" would flash a false alarm on every page load.
    vi.stubGlobal('fetch', vi.fn(() => new Promise(() => {})))
    const { useMetrics } = await import('./useMetrics')
    const { connection, start } = useMetrics()

    expect(connection.value).toBe('unknown')
    start()
    await settle()
    expect(connection.value).toBe('unknown')
  })

  it('reports `online` once a probe has succeeded', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ok()))
    const { useMetrics } = await import('./useMetrics')
    const { connection, start } = useMetrics()

    start()
    await settle()

    expect(connection.value).toBe('online')
  })

  it('reports `offline` when a probe fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('Failed to fetch')
      }),
    )
    const { useMetrics } = await import('./useMetrics')
    const { connection, start } = useMetrics()

    start()
    await settle()

    expect(connection.value).toBe('offline')
  })

  it('goes back to `online` on its own when the core answers again', async () => {
    // The property that gives the indicator its worth: a device that comes
    // back three minutes later — long after the waiting cap of a reboot has
    // expired and its toast has gone — turns the badge green again without a
    // page reload.
    let up = false
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        if (!up) throw new TypeError('Failed to fetch')
        return ok()
      }),
    )
    const { useMetrics } = await import('./useMetrics')
    const { connection, start } = useMetrics()

    start()
    await settle()
    expect(connection.value).toBe('offline')

    up = true
    await vi.advanceTimersByTimeAsync(5000)

    expect(connection.value).toBe('online')
  })

  it('reports `offline` while a power action holds probing paused', async () => {
    // A confirmed shutdown or reboot stops the shared probe (see `pause`), so
    // `unavailable` keeps the value of the last successful probe — `false`.
    // Without reading the pause, the badge would claim "online" for a machine
    // that is switching off, and would keep claiming it forever after a
    // poweroff: that path never calls `resume()`, the device coming back only
    // through a physical gesture.
    vi.stubGlobal('fetch', vi.fn(async () => ok()))
    const { useMetrics } = await import('./useMetrics')
    const { connection, start, pause } = useMetrics()

    start()
    await settle()
    expect(connection.value).toBe('online')

    pause()

    expect(connection.value).toBe('offline')
  })

  it('reports what it measures again once probing resumes', async () => {
    // Every exit path of the reboot wait calls `resume()` — success, expiry
    // of the cap, and leaving the page mid-wait. So the paused state can
    // never stick, and the badge goes back to reporting a measured fact.
    vi.stubGlobal('fetch', vi.fn(async () => ok()))
    const { useMetrics } = await import('./useMetrics')
    const { connection, start, pause, resume } = useMetrics()

    start()
    await settle()
    pause()
    expect(connection.value).toBe('offline')

    resume()
    await settle()

    expect(connection.value).toBe('online')
  })
})

describe('useMetrics staleUi', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
    vi.useFakeTimers()
  })

  afterEach(async () => {
    const { resetMetrics } = await import('./useMetrics')
    resetMetrics()
    vi.useRealTimers()
  })

  it('raises staleUi when the core answers with a different version', async () => {
    // A core that restarted on a new version is a core whose assets may have
    // moved: the page is still running the bundle of the previous version,
    // and nothing reloads it. Two samples of the same version must NOT raise
    // it — that would put the banner on screen on every ordinary poll.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(systemResponse({ version: '0.2.0' }))
      .mockResolvedValueOnce(systemResponse({ version: '0.2.0' }))
      .mockResolvedValue(systemResponse({ version: '0.2.1' }))
    vi.stubGlobal('fetch', spy)
    const { useMetrics } = await import('./useMetrics')
    const { start, staleUi } = useMetrics()
    start()
    await vi.advanceTimersByTimeAsync(0)
    expect(staleUi.value).toBe(false)
    await vi.advanceTimersByTimeAsync(5000)
    expect(staleUi.value).toBe(false)
    await vi.advanceTimersByTimeAsync(5000)
    expect(staleUi.value).toBe(true)
  })

  it('never lowers staleUi once raised', async () => {
    // A flapping answer must not make the banner blink: the page's code is
    // stale for good once the core has restarted on a new version, whatever
    // comes next.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(systemResponse({ version: '0.2.0' }))
      .mockResolvedValueOnce(systemResponse({ version: '0.2.1' }))
      .mockResolvedValue(systemResponse({ version: '0.2.0' }))
    vi.stubGlobal('fetch', spy)
    const { useMetrics } = await import('./useMetrics')
    const { start, staleUi } = useMetrics()
    start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(5000)
    expect(staleUi.value).toBe(true)
    await vi.advanceTimersByTimeAsync(5000)
    expect(staleUi.value).toBe(true)
  })

  it('does not raise staleUi on a plain restart that keeps the same version', async () => {
    // Major B: `session` is minted fresh on *every* process start — a plain
    // "Redémarrer Ritornello", a crash-restart, a reboot of the Pi — and none
    // of those move the version or the bundled UI. Before the fix, the latch
    // watched `session` and this scenario raised the banner on a restart that
    // served the exact same assets, right next to the "Ritornello a
    // redémarré" toast — an outright lie.
    const spy = vi
      .fn()
      .mockResolvedValueOnce(systemResponse({ session: 'a', version: '0.2.0' }))
      .mockResolvedValue(systemResponse({ session: 'b', version: '0.2.0' }))
    vi.stubGlobal('fetch', spy)
    const { useMetrics } = await import('./useMetrics')
    const { start, staleUi } = useMetrics()
    start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(5000)
    expect(staleUi.value).toBe(false)
  })
})
