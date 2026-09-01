import { Dialog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import CountryPicker from './CountryPicker.vue'
import RadioAdmin from './RadioAdmin.vue'

const CATALOG = {
  btn_add: 'Ajouter', btn_save: 'Enregistrer', btn_search: 'Chercher',
  btn_add_result: '+', saved: 'Enregistré', save_error: 'Échec : ',
  limit_reached: '99 maximum', empty_query: 'Saisir un terme',
  searching: 'Recherche…', no_results: 'Aucun résultat',
  col_num: 'N°', col_name: 'Nom', col_url: 'URL',
  search_title: 'Annuaire', search_placeholder: 'nom', country_label: 'Country',
  country_all: 'Tous', country_filter_placeholder: 'Country ou code',
  country_none: 'Aucun country', country_loading: 'Chargement…',
  reorder_hint: 'Glisser', move_up: 'Monter', move_down: 'Descendre',
  load_error_1: 'Erreur : ', load_error_2: '',
}

// Absolute prefix the shell passes via the (required) `base` prop: that's
// the contract, this view does not know the name under which it is served.
const BASE = '/plugins/radio/'

function responses(data: unknown) {
  return vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') return new Response(null, { status: 204 })
    return new Response(JSON.stringify(data), { status: 200 })
  })
}

async function mountLoaded(data: unknown = { stations: [], search: [] }) {
  const spy = responses(data)
  vi.stubGlobal('fetch', spy)
  const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, spy }
}

describe('RadioAdmin', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('loads stations sorted by preset', async () => {
    const { w } = await mountLoaded({
      stations: [
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 1, name: 'A', url: 'http://a' },
      ],
      search: [],
    })
    const names = w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    expect(names).toEqual(['A', 'B'])
  })

  it('numbers by position and renumbers after removal', async () => {
    const { w } = await mountLoaded({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 3, name: 'C', url: 'http://c' },
      ],
      search: [],
    })
    await w.findAll('[data-station-delete]')[0]!.trigger('click')
    expect(w.findAll('[data-station-num]').map((n) => n.text())).toEqual(['1', '2'])
    expect(w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value))
      .toEqual(['B', 'C'])
  })

  it('accepts a tenth station: the bound now follows the server (1..=99)', async () => {
    const stations = Array.from({ length: 9 }, (_, i) => ({
      preset: i + 1, name: `S${i}`, url: `http://s${i}`,
    }))
    const { w } = await mountLoaded({ stations, search: [] })
    await w.find('[data-add]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(10)
  })

  it('rejects a hundredth station with a message', async () => {
    const stations = Array.from({ length: 99 }, (_, i) => ({
      preset: i + 1, name: `S${i}`, url: `http://s${i}`,
    }))
    const { w } = await mountLoaded({ stations, search: [] })
    await w.find('[data-add]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(99)
    expect(w.text()).toContain('99 maximum')
  })

  it('sends the preset derived from position on save', async () => {
    const { w, spy } = await mountLoaded({
      stations: [{ preset: 1, name: 'A', url: 'http://a' }],
      search: [],
    })
    await w.find('[data-add]').trigger('click')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const call = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((call![1] as RequestInit).body))).toEqual({
      op: 'save',
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: '', url: '' },
      ],
    })
  })

  it('searches the directory then rereads the results', async () => {
    const { w, spy } = await mountLoaded({
      stations: [],
      country: 'FR',
      search: [{ name: 'FIP', url: 'http://fip', codec: 'MP3', bitrate: 128, country: 'FR' }],
    })
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((put![1] as RequestInit).body))).toEqual({
      op: 'search', query: 'fip', country: 'FR',
    })
    expect(w.text()).toContain('FIP')
    expect(w.text()).toContain('128')
  })

  it('picks up the country remembered by the plugin and shows it translated', async () => {
    // Fixed bug: the label used to come from the `Select` component, which
    // captures the selected element's text on first render — but
    // `PluginView` mounts the UI with an **empty** catalog, so the page
    // displayed the translation key itself ("country_fr"). The label is
    // now rendered from code, via `Intl.DisplayNames`.
    const { w } = await mountLoaded({ stations: [], search: [], country: 'DE' })
    expect(w.find('[data-country-open]').text()).toBe('Germany')
  })

  it('shows "all countries" when no country is remembered', async () => {
    // Empty string = legitimate choice, not absence of value: that's what
    // the plugin expects in `country`.
    const { w, spy } = await mountLoaded({ stations: [], search: [], country: '' })
    expect(w.find('[data-country-open]').text()).toBe('Tous')
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((put![1] as RequestInit).body))).toEqual({
      op: 'search', query: 'fip', country: '',
    })
  })

  it('only requests the country list when the picker opens, and only once', async () => {
    // A mock faithful to the plugin: `get_data` only renders the list
    // **after** the `countries` operation. A mock that rendered it right
    // from mounting would mask the fetch; a mock that always rendered it
    // empty would give the illusion of a re-fetch on every opening.
    let fetched = false
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        if (JSON.parse(String(init.body)).op === 'countries') fetched = true
        return new Response(null, { status: 204 })
      }
      const body = {
        stations: [],
        search: [],
        countries: fetched ? [{ code: 'BE', stations: 300 }] : [],
      }
      return new Response(JSON.stringify(body), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()

    const puts = () =>
      spy.mock.calls
        .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
        .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    // On page load, no call: nothing justifies it as long as the user
    // isn't trying to change country.
    expect(puts()).toEqual([])

    // `update:open` rather than a real click: a reka Dialog's content is
    // only mounted once it is open, and it's the state that triggers the
    // fetch, not the gesture.
    await w.findComponent(Dialog).vm.$emit('update:open', true)
    await flushPromises()
    expect(puts()).toEqual(['countries'])

    // Closing then reopening requests nothing again: the list is remembered.
    await w.findComponent(Dialog).vm.$emit('update:open', false)
    await w.findComponent(Dialog).vm.$emit('update:open', true)
    await flushPromises()
    expect(puts()).toEqual(['countries'])
  })

  it('the country chosen in the picker goes into the search', async () => {
    const { w, spy } = await mountLoaded({
      stations: [],
      search: [],
      country: '',
      countries: [{ code: 'BE', stations: 300 }],
    })
    await w.findComponent(Dialog).vm.$emit('update:open', true)
    await flushPromises()
    await w.findComponent(CountryPicker).vm.$emit('choose', 'BE')
    await flushPromises()
    expect(w.find('[data-country-open]').text()).toBe('Belgium')

    await w.find('[data-query]').setValue('rock')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)))
      .find((b) => b.op === 'search')
    expect(put).toEqual({ op: 'search', query: 'rock', country: 'BE' })
  })

  it('dragging a station moves it, and the preset follows the position', async () => {
    const { w, spy } = await mountLoaded({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 3, name: 'C', url: 'http://c' },
      ],
      search: [],
    })
    const rows = () => w.findAll('[data-station-row]')
    await rows()[0]!.trigger('dragstart')
    await rows()[2]!.trigger('drop')
    const names = w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    expect(names).toEqual(['B', 'C', 'A'])

    // The preset **is** the position: that's what save sends.
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)))
      .find((b) => b.op === 'save')
    expect(put.stations).toEqual([
      { preset: 1, name: 'B', url: 'http://b' },
      { preset: 2, name: 'C', url: 'http://c' },
      { preset: 3, name: 'A', url: 'http://a' },
    ])
  })

  it('the up/down buttons also move, and are bounded', async () => {
    // Drag-and-drop is neither keyboard-accessible nor reliable with a
    // finger: these buttons are the accessible path, not an ornament.
    const { w } = await mountLoaded({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
      ],
      search: [],
    })
    const names = () =>
      w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    await w.findAll('[data-station-down]')[0]!.trigger('click')
    expect(names()).toEqual(['B', 'A'])
    await w.findAll('[data-station-up]')[1]!.trigger('click')
    expect(names()).toEqual(['A', 'B'])
    // At the extremities, the buttons are disabled.
    expect(w.findAll('[data-station-up]')[0]!.attributes('disabled')).toBeDefined()
    expect(w.findAll('[data-station-down]')[1]!.attributes('disabled')).toBeDefined()
  })

  it('an empty query emits nothing and shows the dedicated message', async () => {
    const { w, spy } = await mountLoaded()
    await w.find('[data-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Saisir un terme')
  })

  it('single flight: a second trigger during a search emits nothing', async () => {
    // The SDK serves admin requests strictly in series: a second search
    // queued behind the first would exceed the core's 5 s cap, which
    // would answer with the translated sentence from its catalog
    // (`plugin_timeout`) rather than a bare code.
    let unblock: () => void = () => {}
    const inProgress = new Promise<void>((r) => (unblock = r))
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        await inProgress
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ stations: [], search: [] }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await w.find('[data-search]').trigger('click')
    await w.find('[data-query]').trigger('keydown', { key: 'Enter' })
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(1)
    unblock()
    await flushPromises()
    // The state is restored: a new search becomes possible again.
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(2)
  })

  it('single flight: the state is restored even after an error', async () => {
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(JSON.stringify({ error: 'annuaire muet' }), { status: 422 })
      return new Response(JSON.stringify({ stations: [], search: [] }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.text()).toContain('annuaire muet')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(2)
  })

  it('addresses its requests under the absolute prefix received via the `base` prop', async () => {
    // IMPORTANT 6 from the final review. This view used to call
    // `api.get('./api/data')` relatively, so resolved against the browser's
    // URL and not against anything the contract guarantees: on
    // `/plugins/radio` (without a trailing slash, a form the shell's
    // router also accepted), `./api/data` resolved to `/plugins/api/data`
    // — which the core interprets as the "api" plugin: 404, empty table
    // and every button failing.
    const spy = responses({ stations: [{ preset: 1, name: 'A', url: 'http://a' }], search: [] })
    vi.stubGlobal('fetch', spy)
    // Deliberately a prefix that is **not** `/plugins/radio/`: the name
    // under which a plugin is served comes from `plugins.toml`, i.e. from
    // deployment. This test would fail if the view rebuilt its own name
    // instead of honoring the received prefix.
    const w = mount(RadioAdmin, {
      props: { catalog: CATALOG, base: '/plugins/tuner/' },
    })
    await flushPromises()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    // Every request, GET as well as PUT, goes out on the absolute URL.
    expect(spy.mock.calls.length).toBeGreaterThan(1)
    for (const call of spy.mock.calls) {
      expect(call[0]).toBe('/plugins/tuner/api/data')
    }
  })

  // --- Failed-load guard (CRITICAL 1 from the final review) ---
  //
  // The old page used to end its load `catch` with
  // `document.querySelectorAll('button').forEach(b => b.disabled = true)`.
  // Without this guard, a failed GET leaves `stations` empty and "Save"
  // active: the PUT `{op:'save', stations: []}` is accepted by
  // `Stations::validate` (which iterates over an empty vector) and
  // **overwrites stations.toml** — every preset lost, with no
  // confirmation. Reachable: the plugin serves admin requests strictly in
  // series with a 4 s directory budget against the core's 5 s cap, so a
  // concurrent load during a search can make the GET fail while a later
  // PUT succeeds (a plugin restart between the two produces the same
  // effect).
  function failedLoad() {
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(null, { status: 204 })
      // The initial GET fails: `api.get` throws on a non-ok status.
      return new Response('unavailable', { status: 503 })
    })
    vi.stubGlobal('fetch', spy)
    return spy
  }

  it('failed load: the three action buttons are disabled', async () => {
    const spy = failedLoad()
    const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.text()).toContain('Erreur : ')
    for (const marker of ['[data-add]', '[data-save]', '[data-search]']) {
      expect((w.find(marker).element as HTMLButtonElement).disabled, marker).toBe(true)
    }
    // Nothing went out: the load failed, no write should have happened
    // from mounting alone.
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('failed load: a click on "Save" emits no request', async () => {
    const spy = failedLoad()
    const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    // `dispatchEvent` rather than VTU's `trigger()`: the latter gives up
    // on its own on a `disabled` element, which would make this test pass
    // without exercising any guard in the view's code. We dispatch the
    // click directly instead, which does call the `@click` handler: it is
    // `save()`'s **early return** that is tested here, not the button's
    // visual state (belt and braces: the protection must not rest on the
    // `disabled` attribute alone).
    w.find('[data-save]').element.dispatchEvent(new Event('click'))
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('failed load: the Enter key in search emits no request and keeps the error message', async () => {
    // Fix folded in (final review): `:disabled` on the "Chercher" button
    // does not protect `@keydown.enter="search"`, which still reached
    // `search()`. A successful search there would do `message.value = ''`,
    // erasing the load error message while `loadFailed` stays true -- the
    // page would look healthy while it is inert. `search()` must therefore
    // carry the same early return as `save()`.
    const spy = failedLoad()
    const w = mount(RadioAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.text()).toContain('Erreur : ')
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-query]').trigger('keydown', { key: 'Enter' })
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    // The load error message remains: it was not erased by a `search()`
    // that would have run anyway.
    expect(w.text()).toContain('Erreur : ')
  })

  it('adds a search result to the table being edited', async () => {
    const { w } = await mountLoaded({
      stations: [],
      search: [{ name: 'FIP', url: 'http://fip', codec: 'MP3', bitrate: 128, country: 'FR' }],
    })
    await w.find('[data-add-result]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(1)
    expect((w.find('[data-station-name]').element as HTMLInputElement).value).toBe('FIP')
  })
})
