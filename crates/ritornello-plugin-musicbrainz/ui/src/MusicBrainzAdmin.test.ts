import { toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import MusicBrainzAdmin from './MusicBrainzAdmin.vue'

// Same approach as `MpdAdmin.test.ts`: keep the real module (components,
// `api`, ...) and replace only the two `toast` entries this view uses, so
// they can be observed without showing a notification.
vi.mock('@ritornello/ui', async () => {
  const real = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...real, toast: { ...real.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOG = {
  title: 'ICY split patterns',
  intro: 'One entry per station this device has probed.',
  col_station: 'Stream',
  col_pattern: 'Pattern',
  col_origin: 'Origin',
  col_last_used: 'Last used',
  col_split_count: 'Titles split',
  col_actions: '',
  origin_standard: 'standard, confirmed',
  origin_learned: 'learned deviation',
  origin_manual: 'manual',
  pattern_no_split: 'do not split',
  pattern_artist_first: 'artist first',
  pattern_title_first: 'title first',
  pattern_title_middle: 'title in the middle field',
  filter_exceptions_only: 'Exceptions only',
  empty: 'No station probed yet.',
  empty_filtered: 'No exception: every probed station follows the standard format.',
  edit: 'Edit',
  delete: 'Delete',
  clear_all: 'Clear all',
  save: 'Save',
  cancel: 'Cancel',
  field_separator: 'Separator',
  field_order: 'Order',
  field_no_split: 'Do not split this station',
  separator_empty: 'the separator cannot be empty',
  separator_no_space: 'the separator must contain a space on each side, otherwise a hyphenated name gets cut in two',
  unknown_station: 'no entry for that stream',
  save_failed: 'could not write the pattern file',
}

// Absolute prefix the shell passes through the (required) `base` prop: that is
// the contract, this view does not know the name under which it is served.
const BASE = '/plugins/musicbrainz/'

const COMPLIANT_STATION = {
  url: 'http://icecast.radiofrance.fr/franceinter-midfi.mp3',
  pattern: { split: { separator: ' - ', artist_first: true } },
  origin: 'standard_confirmed',
  last_used: '2026-08-26T15:32:09Z',
  split_titles: 214,
}

const EXCEPTION_STATION = {
  url: 'http://example/chatter.mp3',
  pattern: 'do_not_split',
  origin: 'learned_deviation',
  last_used: null,
  split_titles: 0,
}

/** Mounts the component with a spied `fetch`: `data` serves the GET, PUTs are
 *  logged in `puts` and answer 204 unless `putResponse` is provided (to
 *  simulate a refusal). */
async function mountView(
  data: { stations: unknown[] } = { stations: [COMPLIANT_STATION, EXCEPTION_STATION] },
  putResponse?: (body: { action: string }) => Response,
) {
  const puts: Array<{ url: string; body: Record<string, unknown> }> = []
  const gets: string[] = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      const body = JSON.parse(String(init.body)) as { action: string }
      puts.push({ url, body })
      if (putResponse) return putResponse(body)
      return new Response(null, { status: 204 })
    }
    gets.push(url)
    return new Response(JSON.stringify(data), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(MusicBrainzAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, spy, puts, gets }
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
})

describe('MusicBrainzAdmin', () => {
  it('hides compliant stations by default', async () => {
    const { w } = await mountView()

    // The filter is active from the first render (no click needed): without
    // this checked box the compliant station would remain visible on first
    // load.
    expect((w.get('[data-filter-exceptions]').element as HTMLInputElement).checked).toBe(true)

    const rows = w.findAll('[data-station-row]')
    expect(rows).toHaveLength(1)
    expect(w.text()).toContain('chatter.mp3')
    expect(w.text()).not.toContain('franceinter-midfi.mp3')
  })

  it('shows them when the filter is unchecked', async () => {
    const { w } = await mountView()

    await w.get('[data-filter-exceptions]').setValue(false)

    const rows = w.findAll('[data-station-row]')
    expect(rows).toHaveLength(2)
    expect(w.text()).toContain('franceinter-midfi.mp3')
    expect(w.text()).toContain('chatter.mp3')
  })

  it('distinguishes "nothing probed" from "no exception"', async () => {
    // Nothing has ever been probed: the raw list is empty.
    const { w: empty } = await mountView({ stations: [] })
    expect(empty.find('[data-empty]').exists()).toBe(true)
    expect(empty.find('[data-empty-filtered]').exists()).toBe(false)
    expect(empty.text()).toContain('No station probed yet.')

    // Stations have been probed, but all are compliant: the filter (active by
    // default) hides them all, which is the opposite information to "nothing
    // probed".
    const { w: filtered } = await mountView({ stations: [COMPLIANT_STATION] })
    expect(filtered.find('[data-empty-filtered]').exists()).toBe(true)
    expect(filtered.find('[data-empty]').exists()).toBe(false)
    expect(filtered.text()).toContain('No exception: every probed station follows the standard format.')
  })

  it('"do not split" greys out the separator and the order', async () => {
    const { w } = await mountView()

    await w.get('[data-edit]').trigger('click')

    const separatorField = w.get('[data-separator]').element as HTMLInputElement
    const orderField = w.get('[data-order]').element as HTMLSelectElement
    // The exception station is already `do_not_split`: the fields are thus
    // already greyed out on opening.
    expect(separatorField.disabled).toBe(true)
    expect(orderField.disabled).toBe(true)

    // Unchecking makes the fields editable again.
    await w.get('[data-do-not-split]').setValue(false)
    expect((w.get('[data-separator]').element as HTMLInputElement).disabled).toBe(false)
    expect((w.get('[data-order]').element as HTMLSelectElement).disabled).toBe(false)

    // And rechecking greys both out again.
    await w.get('[data-do-not-split]').setValue(true)
    expect((w.get('[data-separator]').element as HTMLInputElement).disabled).toBe(true)
    expect((w.get('[data-order]').element as HTMLSelectElement).disabled).toBe(true)
  })

  it('posts a set action with a pattern from the closed set', async () => {
    const { w, puts } = await mountView()

    await w.get('[data-edit]').trigger('click')
    // The exception station opens with "do not split" checked: it must be
    // unchecked to reach the split pattern.
    await w.get('[data-do-not-split]').setValue(false)
    await w.get('[data-separator]').setValue(' :: ')
    await w.get('[data-order]').setValue('title_first')
    await w.get('[data-save-edit]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.url).toBe('/plugins/musicbrainz/api/data')
    // A pattern from the closed set: never a regular expression field, just a
    // separator and an order boolean.
    expect(puts[0]!.body).toEqual({
      action: 'set',
      url: 'http://example/chatter.mp3',
      pattern: { split: { separator: ' :: ', artist_first: false, title_in_middle: false } },
    })
  })

  it('saving without changing anything preserves the three-field shape', async () => {
    // The regression this test exists to prevent, found in review: the form
    // does not **offer** the "Artist - Title - Album" shape — it is only
    // obtained by probing — but it must **replay** it when it was opened on an
    // entry that carries it.
    //
    // Without that, the operator who clicks "Edit" to look and then "Save"
    // without touching anything degraded the pattern: the album got glued
    // back onto the title from the next track on, and since the entry became
    // manual, nothing could repair it anymore. The destructive gesture was not
    // setting this shape, it was saving without modification.
    const { w, puts } = await mountView({
      stations: [
        {
          url: 'http://example/trois-champs.mp3',
          pattern: { split: { separator: ' - ', artist_first: true, title_in_middle: true } },
          origin: 'learned_deviation',
          last_used: '2026-08-26T12:00:00Z',
          split_titles: 7,
        },
      ],
    })

    // The column names it, instead of displaying it like the standard.
    expect(w.get('[data-station-row]').text()).toContain('title in the middle field')

    await w.get('[data-edit]').trigger('click')
    await w.get('[data-save-edit]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.body).toEqual({
      action: 'set',
      url: 'http://example/trois-champs.mp3',
      pattern: { split: { separator: ' - ', artist_first: true, title_in_middle: true } },
    })
  })

  it('posts a remove action, then refreshes', async () => {
    const { w, puts, gets } = await mountView()
    const getsBefore = gets.length

    await w.get('[data-remove]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.body).toEqual({ action: 'remove', url: 'http://example/chatter.mp3' })
    // Removal refreshes the list: a second GET leaves after the PUT.
    expect(gets.length).toBeGreaterThan(getsBefore)
  })

  it('shows the backend error as is', async () => {
    const serverMessage = 'no entry for that stream'
    const { w } = await mountView(
      { stations: [EXCEPTION_STATION] },
      () => new Response(JSON.stringify({ error: serverMessage }), { status: 422 }),
    )

    await w.get('[data-remove]').trigger('click')
    await flushPromises()

    // The displayed message is exactly the text returned by the server —
    // already a translated sentence on the Rust side — never retranslated nor
    // replaced by a JS exception.
    expect(toast.error).toHaveBeenCalledWith(serverMessage)
  })

  it('the Clear button posts a clear action and refreshes', async () => {
    const { w, puts, gets } = await mountView()
    const getsBefore = gets.length

    await w.get('[data-clear]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.body).toEqual({ action: 'clear' })
    expect(gets.length).toBeGreaterThan(getsBefore)
  })
})
