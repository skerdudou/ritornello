import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { defineComponent, h } from 'vue'
import type { UpdatePayload } from '../types'
import { CHECK_FRESH_S, useUpdateCheck, type CheckState } from './useUpdateCheck'

const NOW = 1_800_000_000
const TICK = 2000
const CEILING_TICKS = 10

/** What the fake core answers, per test. */
let checkStatus = 202
let served: Partial<UpdatePayload> = {}
let calls: string[] = []

function payload(over: Partial<UpdatePayload> = {}): UpdatePayload {
  return {
    outcome: { kind: 'ok' },
    release_version: null,
    release_url: null,
    last_check_unix_s: NOW,
    components: [],
    major_update_waiting: false,
    busy: null,
    last_rollback: null,
    ...over,
  }
}

beforeEach(() => {
  checkStatus = 202
  served = {}
  calls = []
  // Only the interval is faked: `flushPromises` needs the real timers.
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] })
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push(`${init?.method ?? 'GET'} ${url}`)
      if (url === '/api/update/check') return new Response('', { status: checkStatus })
      if (url === '/api/update') return new Response(JSON.stringify(payload(served)), { status: 200 })
      return new Response('', { status: 404 })
    }),
  )
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

const posts = () => calls.filter((c) => c === 'POST /api/update/check')

const NEVER: Partial<CheckState> = { outcome: { kind: 'never_checked' }, lastCheckUnixS: null }

/**
 * Mounts a bare component around the composable, the way a dialog does.
 * `state` is what the device answers `GET /api/update` with when the dialog
 * opens (the composable decides on that); `page` is the page's own, possibly
 * stale, copy it only falls back to.
 */
function harness(open: boolean, state: Partial<CheckState> = {}, page: Partial<CheckState> = state) {
  const device = { outcome: { kind: 'ok' }, lastCheckUnixS: NOW, busy: null, ...state } as CheckState
  served = { outcome: device.outcome, last_check_unix_s: device.lastCheckUnixS, busy: device.busy }
  const settled = vi.fn()
  const pageState = { outcome: { kind: 'ok' }, lastCheckUnixS: NOW, busy: null, ...page } as CheckState
  let api!: ReturnType<typeof useUpdateCheck>
  const w = mount(
    defineComponent({
      props: { open: Boolean },
      setup(p) {
        api = useUpdateCheck({
          open: () => p.open,
          state: () => pageState,
          onSettled: settled,
          nowS: () => NOW,
        })
        return () => h('div')
      },
    }),
    { props: { open } },
  )
  return { w, settled, get api() { return api } }
}

const tick = (n = 1) => vi.advanceTimersByTimeAsync(TICK * n)

describe('useUpdateCheck', () => {
  it('enqueues exactly one check on a never-checked state, and waits for it to land', async () => {
    const h1 = harness(true, NEVER)
    await flushPromises()
    expect(posts()).toHaveLength(1)
    expect(h1.api.phase.value).toBe('checking')

    // Not landed yet: neither the timestamp nor the outcome moved.
    await tick()
    expect(h1.api.phase.value).toBe('checking')
    expect(h1.settled).not.toHaveBeenCalled()

    served = { last_check_unix_s: NOW }
    await tick()
    expect(h1.api.phase.value).toBe('idle')
    expect(h1.settled).toHaveBeenCalledTimes(1)
    expect(posts()).toHaveLength(1)
  })

  it('does not enqueue when the last check is under an hour old, and does when it is older', async () => {
    const recent = harness(true, { lastCheckUnixS: NOW - (CHECK_FRESH_S - 60) })
    await flushPromises()
    expect(posts()).toHaveLength(0)
    expect(recent.api.phase.value).toBe('idle')
    recent.w.unmount()

    const stale = harness(true, { lastCheckUnixS: NOW - (CHECK_FRESH_S + 60) })
    await flushPromises()
    expect(posts()).toHaveLength(1)
    expect(stale.api.phase.value).toBe('checking')
  })

  // M-a: the Pi's clock ahead of the browser's gives a negative age, which
  // must not read as "fresh" for hours.
  it('treats a last check dated in the future as stale', async () => {
    harness(true, { lastCheckUnixS: NOW + 600 })
    await flushPromises()
    expect(posts()).toHaveLength(1)
  })

  it('checks again when the last outcome is a failure, however recent', async () => {
    harness(true, { outcome: { kind: 'failed', detail: 'boom' }, lastCheckUnixS: NOW - 10 })
    await flushPromises()
    expect(posts()).toHaveLength(1)
  })

  it('tells the page when it opens on a check that is already fresh, since the page may be behind', async () => {
    const h1 = harness(true, { lastCheckUnixS: NOW - 10 })
    await flushPromises()
    expect(h1.settled).toHaveBeenCalledTimes(1)
  })

  it('only waits when a job is already running, and settles when it clears', async () => {
    const h1 = harness(true, { ...NEVER, busy: 'Checking…' })
    await flushPromises()
    expect(posts()).toHaveLength(0)
    expect(h1.api.phase.value).toBe('checking')

    await tick()
    expect(h1.settled).not.toHaveBeenCalled()

    served = { ...NEVERS(), busy: null }
    await tick()
    expect(h1.settled).toHaveBeenCalledTimes(1)
    expect(posts()).toHaveLength(0)
  })

  // I1: the decision is taken on the device's answer, not on the page's copy.
  // The page still says "never checked, idle" while the device is already
  // running a check (the update card's Check pressed a moment ago, or this
  // dialog closed and reopened while its own check was in flight).
  it('decides on the device, not on the page copy: a check already running is not enqueued again', async () => {
    const h1 = harness(true, { ...NEVER, busy: 'Checking…' }, NEVER)
    await flushPromises()
    expect(posts()).toHaveLength(0)
    expect(h1.api.phase.value).toBe('checking')
  })

  it('decides on the device, not on the page copy: a check that already landed is not enqueued again', async () => {
    harness(true, { lastCheckUnixS: NOW - 10 }, NEVER)
    await flushPromises()
    expect(posts()).toHaveLength(0)
  })

  it('does not enqueue a second check when closed and reopened while its own check is still running', async () => {
    const h1 = harness(true, NEVER)
    await flushPromises()
    expect(posts()).toHaveLength(1)
    await h1.w.setProps({ open: false })
    // The device is running the check; the page never learnt of it.
    served = { ...NEVERS(), busy: 'Checking…' }
    await h1.w.setProps({ open: true })
    await flushPromises()
    expect(posts()).toHaveLength(1)
    expect(h1.api.phase.value).toBe('checking')
  })

  it('reports a full queue (429) as its own phase, not as an error', async () => {
    checkStatus = 429
    const h1 = harness(true, NEVER)
    await flushPromises()
    expect(h1.api.phase.value).toBe('queue_full')
    // And it does not poll for a check that was never queued.
    const before = calls.filter((c) => c === 'GET /api/update').length
    await tick(2)
    expect(calls.filter((c) => c === 'GET /api/update')).toHaveLength(before)
  })

  it('reports any other refusal as an error, and Retry enqueues again', async () => {
    checkStatus = 500
    const h1 = harness(true, NEVER)
    await flushPromises()
    expect(h1.api.phase.value).toBe('error')
    expect(h1.api.error.value).toContain('500')

    checkStatus = 202
    await h1.api.retry()
    expect(posts()).toHaveLength(2)
    expect(h1.api.phase.value).toBe('checking')
  })

  it('stops waiting when closed', async () => {
    const h1 = harness(true, NEVER)
    await flushPromises()
    await h1.w.setProps({ open: false })
    served = { last_check_unix_s: NOW + 1 }
    await tick(2)
    expect(h1.settled).not.toHaveBeenCalled()
  })

  describe('what ends the wait', () => {
    // One clause at a time: each test moves exactly one signal, so a mutant
    // that drops that clause leaves the wait running.
    it('the timestamp alone', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS(), last_check_unix_s: NOW }
      await tick()
      // `outcome` is unchanged (never_checked), busy was never seen.
      expect(h1.settled).toHaveBeenCalledTimes(1)
      expect(h1.api.phase.value).toBe('idle')
    })

    it('the outcome alone', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { outcome: { kind: 'no_release' }, last_check_unix_s: null }
      await tick()
      expect(h1.settled).toHaveBeenCalledTimes(1)
    })

    it('a busy state that came and went, with nothing else moved', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS(), busy: 'Checking…' }
      await tick()
      expect(h1.settled).not.toHaveBeenCalled()
      served = { ...NEVERS(), busy: null }
      await tick()
      expect(h1.settled).toHaveBeenCalledTimes(1)
    })

    it('never busy and nothing moved: still waiting one tick before the ceiling', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS() }
      await tick(CEILING_TICKS - 1)
      expect(h1.settled).not.toHaveBeenCalled()
      expect(h1.api.phase.value).toBe('checking')
    })
  })

  // I2: the ceiling is a visible state, not a silent stop.
  describe('the ceiling', () => {
    it('turns into "still looking" at exactly the tenth tick, and not before', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS() }
      await tick(CEILING_TICKS - 1)
      expect(h1.api.phase.value).toBe('checking')
      await tick()
      expect(h1.api.phase.value).toBe('timeout')
      expect(h1.settled).not.toHaveBeenCalled()
    })

    it('keeps looking, and settles when the check lands late', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS() }
      await tick(CEILING_TICKS)
      expect(h1.api.phase.value).toBe('timeout')

      served = { last_check_unix_s: NOW }
      await tick()
      expect(h1.api.phase.value).toBe('idle')
      expect(h1.settled).toHaveBeenCalledTimes(1)
    })

    it('ends on the failure when the check failed the same way as before', async () => {
      const failed: CheckState['outcome'] = { kind: 'failed', detail: 'boom' }
      const h1 = harness(true, { outcome: failed, lastCheckUnixS: null })
      await flushPromises()
      served = { outcome: failed, last_check_unix_s: null }
      await tick(CEILING_TICKS)
      expect(h1.api.phase.value).toBe('idle')
      expect(h1.settled).toHaveBeenCalledTimes(1)
    })

    it('Retry from the timeout re-reads the device and enqueues nothing while a check is running', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS(), busy: 'Checking…' }
      await tick(CEILING_TICKS)
      expect(h1.api.phase.value).toBe('timeout')
      await h1.api.retry()
      expect(posts()).toHaveLength(1)
      expect(h1.api.phase.value).toBe('checking')
    })

    it('Retry from the timeout enqueues again when the device shows no check running and none landed', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS() }
      await tick(CEILING_TICKS)
      await h1.api.retry()
      expect(posts()).toHaveLength(2)
    })

    it('Retry from the timeout enqueues nothing when the check landed meanwhile', async () => {
      const h1 = harness(true, NEVER)
      await flushPromises()
      served = { ...NEVERS() }
      await tick(CEILING_TICKS)
      served = { last_check_unix_s: NOW - 5 }
      await h1.api.retry()
      expect(posts()).toHaveLength(1)
      expect(h1.api.phase.value).toBe('idle')
    })
  })
})

/** The never-checked device state, as a served payload. */
function NEVERS(): Partial<UpdatePayload> {
  return { outcome: { kind: 'never_checked' }, last_check_unix_s: null, busy: null }
}
