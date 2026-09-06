import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

/**
 * The badge reads the shared probe of `useMetrics` directly, like `BottomNav`
 * reads `usePlugins`. So these tests drive it the way the running page does —
 * through the answers of `/api/system` — rather than by handing it a state as
 * a prop. It costs a `fetch` stub and proves the whole chain, from the probe's
 * answer to the rendered label.
 */

const CATALOG = {
  connection_online: 'Online',
  connection_offline: 'Offline',
  connection_unknown: 'Connecting…',
}

/** Routes the two requests a mounted badge triggers: the catalog, and the
 *  system probe whose fate the test is actually choosing. */
function stubFetch(system: () => Promise<Response>) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (String(url).includes('/api/i18n')) {
        return new Response(JSON.stringify(CATALOG), { status: 200 })
      }
      return system()
    }),
  )
}

async function mountWith(system: () => Promise<Response>) {
  vi.resetModules()
  stubFetch(system)
  const { useCatalog } = await import('../composables/useCatalog')
  await useCatalog().reload()
  const { useMetrics } = await import('../composables/useMetrics')
  useMetrics().start()
  // Let the first probe land (or not, when the test keeps it pending).
  await vi.advanceTimersByTimeAsync(0)
  const ConnectionStatus = (await import('./ConnectionStatus.vue')).default
  return mount(ConnectionStatus)
}

function ok() {
  return new Response(JSON.stringify({ service_uptime_s: 12 }), { status: 200 })
}

describe('ConnectionStatus', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
    vi.useFakeTimers()
  })

  afterEach(async () => {
    const { resetMetrics } = await import('../composables/useMetrics')
    resetMetrics()
    const { resetCatalog } = await import('../composables/useCatalog')
    resetCatalog()
    vi.useRealTimers()
  })

  it('announces the core as reachable once a probe has answered', async () => {
    const w = await mountWith(async () => ok())

    expect(w.get('[data-connection]').attributes('data-connection')).toBe('online')
    expect(w.text()).toContain('Online')
  })

  it('announces the core as unreachable when the probe fails', async () => {
    const w = await mountWith(async () => {
      throw new TypeError('Failed to fetch')
    })

    expect(w.get('[data-connection]').attributes('data-connection')).toBe('offline')
    expect(w.text()).toContain('Offline')
  })

  it('says nothing definite before the first answer', async () => {
    const w = await mountWith(() => new Promise<Response>(() => {}))

    expect(w.get('[data-connection]').attributes('data-connection')).toBe('unknown')
    expect(w.text()).toContain('Connecting…')
  })

  /**
   * The transition is what matters on this badge: a user who is reading
   * something else must be told the device dropped, not have to notice a
   * colour change. `aria-live="polite"` on a `role="status"` region is what
   * carries that.
   */
  it('is a live region, so the drop is announced and not merely painted', async () => {
    const w = await mountWith(async () => ok())

    const badge = w.get('[data-connection]')
    expect(badge.attributes('role')).toBe('status')
    expect(badge.attributes('aria-live')).toBe('polite')
  })

  /**
   * On a phone the header holds the brand and the theme toggle, and the label
   * is dropped for want of room — visually only. `sr-only sm:not-sr-only`
   * keeps the words in the accessibility tree at every width, where `hidden`
   * would take them out of it and leave a screen reader with a coloured dot
   * and nothing to read.
   *
   * jsdom applies no stylesheet, so the breakpoint itself cannot be observed
   * here (and `isVisible()` would happily lie about it). What is asserted is
   * the contract the classes express, plus the fact that the words are in the
   * DOM either way.
   */
  it('keeps the label readable by a screen reader at phone width', async () => {
    const w = await mountWith(async () => ok())

    const label = w.get('[data-connection-label]')
    expect(label.text()).toBe('Online')
    expect(label.classes()).toContain('sr-only')
    expect(label.classes()).toContain('sm:not-sr-only')
    expect(label.classes()).not.toContain('hidden')
  })
})
