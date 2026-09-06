import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const CATALOGUE = {
  cover_cache_open: 'Voir la mémoire occupée par les pochettes',
  cover_cache_title: 'Mémoire occupée par les pochettes',
  cover_cache_hint: 'Relevé au moment de l’ouverture.',
  cover_cache_thumbnails: 'Vignettes',
  cover_cache_full_sizes: 'Pleins formats',
  cover_cache_renditions: 'Réencodages',
  cover_cache_used: 'Occupé',
  cover_cache_kio: '{n} Kio',
  cover_cache_mio: '{n} Mio',
  cover_cache_used_value: '{used} sur {budget}',
  cover_cache_empty: 'Aucune pochette en mémoire.',
  cover_cache_failed: 'Relevé indisponible.',
  reload: 'Recharger',
}

/**
 * The weights add up on purpose: 921,600 + 11,661,312 = 12,582,912, which is
 * the whole promise the panel makes. A fixture where they did not would let a
 * broken total pass unnoticed.
 *
 * The two are also deliberately on either side of one mebibyte, so that a
 * single render exercises both units.
 */
const SNAPSHOT = {
  thumbnails: 18,
  thumbnails_bytes: 921_600,
  full_sizes: 3,
  full_sizes_bytes: 11_661_312,
  renditions_built: 7,
  used_bytes: 12_582_912,
  budget_bytes: 52_428_800,
}

async function mountPanel(payload: unknown = SNAPSHOT, status = 200) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOGUE), { status: 200 })
      if (url === '/api/cover-cache') {
        return new Response(JSON.stringify(payload), { status })
      }
      return new Response('unknown', { status: 404 })
    }),
  )
  const { useCatalog } = await import('../composables/useCatalog')
  const CoverCacheDetails = (await import('./CoverCacheDetails.vue')).default
  document.body.innerHTML = ''
  // The catalog is a module-level singleton that `ConfigView.vue` populates
  // on its own mount in production; mounted standalone here, nothing else
  // reloads it, so it is loaded explicitly before the panel opens -- the
  // component itself must not do this (see CoverCacheDetails.vue for why).
  await useCatalog().reload()
  const w = mount(CoverCacheDetails, { attachTo: document.body })
  await flushPromises()
  return w
}

/** Opens the panel and hands back its text. The dialog is rendered in a
 *  portal, so it lives in `document.body` (same convention as
 *  `SystemView.test.ts` and `PlayerCard.test.ts`). */
async function opened(payload: unknown = SNAPSHOT) {
  const w = await mountPanel(payload)
  await w.find('[data-cover-cache-open]').trigger('click')
  await flushPromises()
  return (selector: string) => document.body.querySelector(selector)
}

describe('CoverCacheDetails', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.unstubAllGlobals()
  })

  it('fetches nothing until the panel is opened', async () => {
    // **Loaded on opening, never polled.** The production change that would
    // break this: an `onMounted` that reads the snapshot, which would make
    // it read on every visit to the settings page, panel closed or not.
    const w = await mountPanel()
    const spy = globalThis.fetch as unknown as ReturnType<typeof vi.fn>
    expect(spy.mock.calls.filter((c) => c[0] === '/api/cover-cache')).toHaveLength(0)

    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => c[0] === '/api/cover-cache')).toHaveLength(1)
  })

  it('lists its lines in the order the panel is meant to be read', async () => {
    // Summary first, then its breakdown: work done, the occupied total, then
    // the two lines that add up to it. The order is a decision the owner made
    // and nothing else in this file would notice it being undone -- every
    // other assertion here reads one line by its own attribute.
    await opened()
    const labels = [...document.body.querySelectorAll('[data-cover-cache-panel] dt')].map((n) =>
      n.textContent?.trim(),
    )
    expect(labels).toEqual(['Réencodages', 'Occupé', 'Pleins formats', 'Vignettes'])
  })

  it('shows each kind held, with its count and its weight', async () => {
    // 921,600 bytes is 900 KiB; 11,661,312 is 11 MiB once rounded. Both lines
    // are asserted, and the full-size one is the one the owner reported
    // missing: a station announcing a single URL lands there and nowhere else.
    const q = await opened()
    const thumbs = q('[data-cover-cache-thumbnails]')?.textContent
    expect(thumbs).toContain('18')
    expect(thumbs).toContain('900 Kio')
    const full = q('[data-cover-cache-full]')?.textContent
    expect(full).toContain('3')
    expect(full).toContain('11 Mio')
    expect(q('[data-cover-cache-renditions]')?.textContent).toContain('7')
    expect(q('[data-cover-cache-used]')?.textContent).toContain('12 Mio sur 50 Mio')
  })

  it('states a small cache in kibibytes rather than rounding it to nothing', async () => {
    // **The regression this rewrite exists to remove.** Fixed at mebibytes,
    // 401,408 bytes printed "0", so the panel claimed nothing was occupied
    // while holding 392 KiB of radio covers -- and that is the ordinary state
    // of this device. The budget stays in mebibytes: it is what the user typed
    // on the card above, and the two must remain comparable.
    const q = await opened({
      ...SNAPSHOT,
      thumbnails: 0,
      thumbnails_bytes: 0,
      full_sizes: 4,
      full_sizes_bytes: 401_408,
      used_bytes: 401_408,
    })
    expect(q('[data-cover-cache-used]')?.textContent).toContain('392 Kio sur 50 Mio')
    expect(q('[data-cover-cache-panel]')?.textContent).not.toContain('Aucune pochette')
  })

  it('fills every number into its phrase rather than printing the placeholder', async () => {
    // The unit travels inside the translated string so a language may place it
    // where its grammar wants. The production change this kills: rendering one
    // of those keys without its value, which shows a literal `{n}` to the user
    // and which no assertion on the surrounding text would notice.
    const q = await opened()
    const panel = q('[data-cover-cache-panel]')?.textContent
    expect(panel).not.toContain('{n}')
    expect(panel).not.toContain('{used}')
    expect(panel).not.toContain('{budget}')
  })

  it('omits a weight beside a count of zero rather than printing "0 Kio"', async () => {
    const q = await opened({ ...SNAPSHOT, thumbnails: 0, thumbnails_bytes: 0, used_bytes: 11_661_312 })
    expect(q('[data-cover-cache-thumbnails]')?.textContent?.trim()).toBe('0')
  })

  it('says nothing is held in memory only when no byte is held', async () => {
    // **"In memory", and judged on the total.** A library of local covers
    // holds hundreds of entries costing a path and no bytes: they show on
    // neither line, and the panel must not call that an empty cache -- but it
    // must still say that no memory is spent, which is its subject.
    const q = await opened({
      ...SNAPSHOT,
      thumbnails: 0,
      thumbnails_bytes: 0,
      full_sizes: 0,
      full_sizes_bytes: 0,
      used_bytes: 0,
    })
    expect(q('[data-cover-cache-panel]')?.textContent).toContain('Aucune pochette en mémoire.')
  })

  it('shows a message rather than an empty panel when the snapshot fails', async () => {
    // `api.get` **throws** on failure, unlike `api.put`. Without a `catch`,
    // opening the panel would produce an unhandled rejection and a mute
    // panel.
    const w = await mountPanel('boom', 500)
    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    expect(document.body.querySelector('[data-cover-cache-error]')).not.toBeNull()
  })

  it('re-reads on demand, and only then', async () => {
    const w = await mountPanel()
    await w.find('[data-cover-cache-open]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-cover-cache-reload]')!.click()
    await flushPromises()
    const spy = globalThis.fetch as unknown as ReturnType<typeof vi.fn>
    expect(spy.mock.calls.filter((c) => c[0] === '/api/cover-cache')).toHaveLength(2)
  })
})
