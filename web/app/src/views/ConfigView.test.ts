import { api, Select, SelectItem, toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

// Same approach as `useTheme.test.ts`: we keep the real module (components,
// `api`, ...) and replace only the two `toast` entries this view uses, so we
// can observe them without showing any notification.
// `api.put` is wrapped (not replaced): the toggle must really go through the
// spied `fetch` below, while staying observable by the tests.
vi.mock('@ritornello/ui', async () => {
  const actual = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return {
    ...actual,
    api: { ...actual.api, put: vi.fn(actual.api.put) },
    toast: { ...actual.toast, error: vi.fn(), success: vi.fn() },
  }
})

const CATALOGUE = {
  config_title: 'Configuration',
  plugins_title: 'Plugins',
  col_plugin: 'Plugin', col_kind: 'Genre', col_state: 'État', col_admin: 'Admin', col_enabled: 'Actif',
  connected: 'connecté', unavailable: 'unavailable', stalled: 'figé', disabled: 'désactivé',
  starting: 'démarrage', busy: 'occupé',
  admin_link: 'admin', toggle_plugin: 'Activer ou désactiver {name}',
  plugin_enabled: '{name} activé.', plugin_disabled: '{name} désactivé.',
  audio_output: 'Sortie audio', audio_default_device: 'Par défaut (système)',
  language: 'Langue', change: 'Changer', ok: 'OK',
  recent_errors: 'Dernières erreurs',
  startup_title: 'Démarrage', startup_on: 'allumé', startup_standby: 'veille',
  clock_title: 'Date et heure', clock_date_label: 'Date', clock_hours_label: 'Heures',
  clock_24h: '24 h (13:05)', clock_12h: '12 h (1:05 PM)',
  clock_date_dmy: '31/12/2026', clock_date_ymd: '2026-12-31', clock_date_mdy: '12/31/2026',
  clock_hint: "Sert à l'horloge de veille des afficheurs.",
  startup_previous: 'état précédent',
  volume_hold_title: 'Volume maintenu',
  volume_hold_initial: 'Délai initial (ms)', volume_hold_interval: 'Intervalle de répétition (ms)',
  overlays_title: 'Incrustations',
  overlay_ms_label: "Durée d'affichage (volume, messages) (ms)",
  tens_window_ms_label: 'Fenêtre de saisie du cumul +10 (ms)',
  seek_card_title: 'Déplacement',
  seek_step_label: 'Pas de déplacement (s)',
  cover_card_title: "Pochettes d'album",
  cover_cache_budget_label: 'Budget mémoire (Mio)',
  cover_cache_budget_help: 'Borne les octets réseau et les vignettes gardées.',
  cover_cache_estimate:
    'Avec {budget} Mio de budget, un plafond de {download} Mio et des entrées d\'au plus {entry} Kio : au moins {floor} pochettes, environ {typical} pour une bibliothèque locale.',
  cover_cache_estimate_unlimited: 'Au moins {floor} pochettes ; le cache en garde quelques centaines au plus.',
  cover_predicted_weight: 'une vignette pèse environ {kio} Kio',
  cover_download_max_label: 'Plafond réseau (Mio)',
  cover_download_max_help: 'Plus grande pochette téléchargée depuis internet.',
  cover_source_max_label: 'Plafond de la source (Mio)',
  cover_source_max_help: 'Toujours appliqué.',
  cover_rendition_label: 'Réencoder les pochettes',
  cover_rendition_help: 'Décoché, la source part telle quelle.',
  cover_max_edge_label: 'Côté le plus long (px)',
  cover_jpeg_quality_label: 'Qualité JPEG',
  cover_jpeg_quality_help: 'JPEG seulement.',
  cover_passthrough_max_label: 'Ne pas réencoder en dessous de (Kio)',
  cover_passthrough_max_help: 'Un seuil.',
  cover_max_pixels_label: 'Plafond de décodage (Mpx)',
  cover_max_pixels_help: 'Lu dans l’en-tête.',
  toc_label: 'sections',
}

/** Payloads served by the fake `fetch`, overridable per test. */
function payloads() {
  return {
    '/api/status': {
      plugins: [
        { name: 'radio', kind: 'source', connected: true, admin: true },
        { name: 'cd', kind: 'source', connected: false, admin: false },
      ],
      active_source: 'radio',
    } as unknown,
    '/api/audio-output': {
      devices: [
        { name: 'hw:CARD=Headphones', description: 'bcm2835 Headphones — Direct hardware device' },
        { name: 'hw:CARD=HDMI', description: '' },
      ],
      current: 'hw:CARD=HDMI',
    } as unknown,
    '/api/locale': { locales: ['en', 'fr'], current: 'fr' } as unknown,
    '/api/logs': { lines: ['WARN plugin radio unavailable'] } as unknown,
    '/api/settings': {
      volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, startup_power: 'on',
      overlay_ms: 5000, tens_window_ms: 5000, seek_step_s: 10,
      cover_cache_budget_mio: 50, cover_download_max_mio: 2,
      cover_source_max_mio: 20, cover_rendition: true, cover_max_edge_px: 640,
      cover_jpeg_quality: 85, cover_passthrough_max_ko: 150, cover_max_pixels_mpx: 16,
    } as unknown,
    '/api/i18n': CATALOGUE as unknown,
  }
}

type Payloads = ReturnType<typeof payloads>

// jsdom does not implement IntersectionObserver: the view needs it for the
// scrollspy, so we replace it with a fake class that captures the callback,
// letting the tests simulate sections entering/leaving the viewport.
type IOCallback = (entries: Array<{ target: Element; isIntersecting: boolean }>) => void
let ioCallback: IOCallback | null = null
class FakeIO {
  constructor(cb: IOCallback) { ioCallback = cb }
  observe() {}
  disconnect() {}
}

/**
 * Mounts ConfigView with an in-memory router (RouterLink is imported directly
 * by the SFC: it needs a real router, which additionally lets us observe the
 * `href` actually resolved) and a spied `fetch`.
 */
async function mountView(overrides: Partial<Payloads> = {}, putError?: string) {
  const table = { ...payloads(), ...overrides }
  const puts: Array<{ url: string; body: unknown }> = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      puts.push({ url, body: JSON.parse(String(init.body)) })
      if (putError) {
        return new Response(JSON.stringify({ error: putError }), { status: 422 })
      }
      return new Response(null, { status: 204 })
    }
    const data = (table as Record<string, unknown>)[url]
    if (data === undefined) return new Response('unknown', { status: 404 })
    return new Response(JSON.stringify(data), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  vi.stubGlobal('IntersectionObserver', FakeIO)

  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: { template: '<div />' } },
      { path: '/config', component: { template: '<div />' } },
      { path: '/plugins/:name/', component: { template: '<div />' } },
    ],
  })
  router.push('/config')
  await router.isReady()

  const ConfigView = (await import('./ConfigView.vue')).default
  // Attached to the real document (not to a detached node, `mount`'s default):
  // the table of contents finds its sections through `document.getElementById`,
  // which sees nothing outside the document tree. We start from an empty body
  // on every mount so that the section ids (unique per payload, but reused
  // between tests) do not point at the previous mount.
  document.body.innerHTML = ''
  const w = mount(ConfigView, { global: { plugins: [router] }, attachTo: document.body })
  await flushPromises()
  return { w, spy, puts, table }
}

/**
 * Sugar for the toggle tests: they only override `/api/status`, unlike
 * `mountView` which expects one payload per URL. Reuses the same mount rather
 * than inventing a second one.
 */
async function mountWithStatus(status: unknown) {
  const { w } = await mountView({ '/api/status': status })
  return w
}

function resetMocks() {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
  vi.mocked(api.put).mockClear()
}

// The largest surface migrated in this project had no unit test at all: four
// Rust tests covered the old server-rendered page and were deleted along with
// it (IMPORTANT 7 of the final review). Nothing exercised the columns of the
// plugin table any more, the connected/unavailable labels, the shape of the
// admin link URL, the audio PUT, the language PUT followed by the catalog
// reload, nor the rendering of the logs — and the empty audio output defect
// (IMPORTANT 3) is precisely the one such a test would have caught.
describe('ConfigView — plugin table', () => {
  beforeEach(resetMocks)

  it('renders one row per plugin with its five columns', async () => {
    const { w } = await mountView()
    const rows = w.findAll('[data-plugin-row]')
    expect(rows).toHaveLength(2)
    expect(rows[0]!.find('[data-plugin-name]').text()).toBe('radio')
    expect(rows[0]!.find('[data-plugin-kind]').text()).toBe('source')
    expect(rows[1]!.find('[data-plugin-name]').text()).toBe('cd')
    expect(rows[1]!.find('[data-plugin-kind]').text()).toBe('source')
    // The five headers are translated from the core catalog.
    const headers = w.findAll('th').map((h) => h.text())
    expect(headers).toEqual(['Plugin', 'Genre', 'État', 'Admin', 'Actif'])
  })

  it('distinguishes the connected state from the unavailable state', async () => {
    const { w } = await mountView()
    const rows = w.findAll('[data-plugin-row]')
    expect(rows[0]!.find('[data-plugin-state]').text()).toBe('connecté')
    expect(rows[1]!.find('[data-plugin-state]').text()).toBe('unavailable')
  })

  it('distinguishes the stalled state (process alive, silent at the deadline) from the other two', async () => {
    // Three situations the core now distinguishes (see /api/status):
    // announced+wired, dead before announcing itself, and alive but silent at
    // the deadline (may still announce itself later, without a restart). The
    // UI must no longer confuse them.
    const { w } = await mountView({
      '/api/status': {
        plugins: [
          { name: 'radio', kind: 'source', connected: true, admin: true },
          { name: 'cd', kind: 'source', connected: false, admin: false },
          { name: 'files', kind: 'source', connected: false, stalled: true, admin: false },
        ],
        active_source: 'radio',
      },
    })
    const rows = w.findAll('[data-plugin-row]')
    const texts = rows.map((l) => l.find('[data-plugin-state]').text())
    expect(texts).toEqual(['connecté', 'unavailable', 'figé'])
    // Three distinct labels...
    expect(new Set(texts).size).toBe(3)
    // ...carried by three distinct badge styles: a mere change of text on the
    // "destructive" color would leave a stalled plugin dressed like a dead
    // plugin.
    const classes = rows.map(
      (l) => l.find('[data-plugin-state] [data-slot="badge"]').classes().join(' '),
    )
    expect(new Set(classes).size).toBe(3)
  })

  it('a busy plugin (reachable, but its page does not answer) reads busy, not connected', async () => {
    // `busy` comes from a ping of the admin page that times out: the plugin is
    // alive, it is wired, but a long `set_data` (network share) holds its lock.
    // "connected" would be true and useless: it is precisely what says nothing.
    const { w } = await mountView({
      '/api/status': {
        plugins: [
          { name: 'files', kind: 'source', connected: true, admin: true, busy: true },
          { name: 'radio', kind: 'source', connected: true, admin: true },
        ],
        active_source: 'radio',
      },
    })
    const rows = w.findAll('[data-plugin-row]')
    const texts = rows.map((l) => l.find('[data-plugin-state]').text())
    expect(texts).toEqual(['occupé', 'connecté'])
    const classes = rows.map(
      (l) => l.find('[data-plugin-state] [data-slot="badge"]').classes().join(' '),
    )
    expect(classes[0]!).not.toBe(classes[1]!)
  })

  it('renders the admin link only for admin plugins, at /plugins/<name>/', async () => {
    const { w } = await mountView()
    const rows = w.findAll('[data-plugin-row]')
    const link = rows[0]!.find('[data-admin-link]')
    expect(link.exists()).toBe(true)
    // The canonical form with a trailing slash: it is the history URL, pinned
    // on the core side too (`serves_shell("/plugins/radio/")`) and now the only
    // one the router lets live.
    expect(link.attributes('href')).toBe('/plugins/radio/')
    expect(link.text()).toBe('admin')
    // "cd" is not admin: no link, a dash in its place.
    expect(rows[1]!.find('[data-admin-link]').exists()).toBe(false)
    expect(rows[1]!.text()).toContain('-')
  })

  it('an empty plugin table does not break the rendering', async () => {
    const { w } = await mountView({ '/api/status': { plugins: [], active_source: '' } })
    expect(w.findAll('[data-plugin-row]')).toHaveLength(0)
    expect(w.text()).toContain('Plugins')
  })

  it('groups the kinds of a same plugin on a single row', async () => {
    // The table must show the unit being manipulated: the toggle applies to
    // the plugin, not to one of its kinds.
    const wrapper = await mountWithStatus({
      plugins: [
        { name: 'files', kind: 'source', connected: true, admin: true },
        { name: 'files', kind: 'metadata', connected: true, admin: true },
        { name: 'cd', kind: 'unknown', connected: false, admin: false, disabled: true },
      ],
      active_source: 'files',
    })
    const rows = wrapper.findAll('[data-plugin-row]')
    expect(rows).toHaveLength(2)
    expect(rows[0]!.find('[data-plugin-kind]').text()).toBe('source, metadata')
  })

  it('toggles a plugin and reloads', async () => {
    const wrapper = await mountWithStatus({
      plugins: [{ name: 'cd', kind: 'source', connected: true, admin: false }],
      active_source: 'cd',
    })
    await wrapper.find('[data-plugin-toggle]').trigger('click')
    await flushPromises()
    expect(api.put).toHaveBeenCalledWith('/api/plugins/cd/enabled', { enabled: false })
  })

  it('says why when the core refuses', async () => {
    vi.mocked(api.put).mockResolvedValueOnce('plugins.toml is read-only')
    const wrapper = await mountWithStatus({
      plugins: [{ name: 'cd', kind: 'source', connected: true, admin: false }],
      active_source: 'cd',
    })
    await wrapper.find('[data-plugin-toggle]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('plugins.toml is read-only')
  })

  // Today, `replace_plugin_lines` on the core side (status.rs) replaces all the
  // lines of a name as one block: either real kinds, or a single synthetic
  // "unknown" — never a mix. This guard does not depend on it: it checks the
  // display for a set of lines the server does not produce yet, in both
  // orders, to prove that the grouping does not rely on arrival order.
  it('never shows unknown next to a real kind: real then unknown', async () => {
    const wrapper = await mountWithStatus({
      plugins: [
        { name: 'x', kind: 'source', connected: true, admin: false },
        { name: 'x', kind: 'unknown', connected: true, admin: false },
      ],
      active_source: '',
    })
    expect(wrapper.find('[data-plugin-kind]').text()).toBe('source')
  })

  it('never shows unknown next to a real kind: unknown then real', async () => {
    const wrapper = await mountWithStatus({
      plugins: [
        { name: 'x', kind: 'unknown', connected: true, admin: false },
        { name: 'x', kind: 'source', connected: true, admin: false },
      ],
      active_source: '',
    })
    expect(wrapper.find('[data-plugin-kind]').text()).toBe('source')
  })

  it('a half-connected plugin does not read as connected', async () => {
    // The existing grouping test alone connects both kinds: without this one,
    // a regression doing an OR instead of an AND would go unnoticed.
    const wrapper = await mountWithStatus({
      plugins: [
        { name: 'files', kind: 'source', connected: true, admin: false },
        { name: 'files', kind: 'metadata', connected: false, admin: false },
      ],
      active_source: 'files',
    })
    expect(wrapper.find('[data-plugin-state]').text()).toBe('unavailable')
  })

  it('encodes the plugin name in the toggle URL', async () => {
    const wrapper = await mountWithStatus({
      plugins: [{ name: 'my plugin', kind: 'source', connected: true, admin: false }],
      active_source: 'my plugin',
    })
    await wrapper.find('[data-plugin-toggle]').trigger('click')
    await flushPromises()
    expect(api.put).toHaveBeenCalledWith('/api/plugins/my%20plugin/enabled', { enabled: false })
  })

  // Fix 4 of the final review: disabling the active source can cost up to 15 s
  // if the incoming or the outgoing one does not answer. Without an in-flight
  // marker, the switch stayed clickable — and clickable twice — during that
  // whole window.
  it('disables the switch while the toggle is in flight, re-enables it afterwards', async () => {
    let resolve: (v: string | null) => void = () => {}
    const inFlight = new Promise<string | null>((r) => { resolve = r })
    vi.mocked(api.put).mockReturnValueOnce(inFlight)
    const wrapper = await mountWithStatus({
      plugins: [{ name: 'cd', kind: 'source', connected: true, admin: false }],
      active_source: 'cd',
    })

    await wrapper.find('[data-plugin-toggle]').trigger('click')
    expect(wrapper.find('[data-plugin-toggle]').attributes('disabled')).toBeDefined()
    // Still in flight: a second click must not double the call.
    await wrapper.find('[data-plugin-toggle]').trigger('click')
    expect(api.put).toHaveBeenCalledTimes(1)

    resolve(null)
    await flushPromises()
    expect(wrapper.find('[data-plugin-toggle]').attributes('disabled')).toBeUndefined()
  })
})

describe('ConfigView — language', () => {
  beforeEach(resetMocks)

  it('sends the language PUT then reloads the catalog', async () => {
    // Changing the language reloads the catalogs instead of reloading the whole
    // page as the old UI did: it is `loadAll()` (and its `reload()`) that
    // replaces `location.reload()`. So the test checks that a second
    // `GET /api/i18n` does follow the PUT.
    const { w, spy, puts } = await mountView()
    const before = spy.mock.calls.filter((c) => c[0] === '/api/i18n').length
    expect(before).toBeGreaterThan(0) // loaded at mount

    await w.findAllComponents(Select)[1]!.vm.$emit('update:modelValue', 'en')
    await w.find('[data-lang-change]').trigger('click')
    await flushPromises()

    expect(puts).toEqual([{ url: '/api/locale', body: { locale: 'en' } }])
    // The catalog was re-read after the PUT — otherwise the UI would stay
    // displayed in the old language until the next manual reload.
    expect(spy.mock.calls.filter((c) => c[0] === '/api/i18n').length).toBeGreaterThan(before)
  })

  it('shows the name of the language and not its code', async () => {
    // "français" is read, "fr" is guessed. The code remains the value sent to
    // the core (checked by the PUT test above).
    const { w } = await mountView()
    const texts = w.findAllComponents(SelectItem).map((i) => i.text())
    expect(texts).toContain('Français')
    expect(texts).toContain('English')
    expect(texts).not.toContain('fr')
  })

  it('a failed language PUT is reported and reloads nothing', async () => {
    const { w, spy } = await mountView({}, 'unknown language')
    const before = spy.mock.calls.filter((c) => c[0] === '/api/i18n').length
    await w.find('[data-lang-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('unknown language')
    // No reload: the language did not change on the server side, re-reading
    // the catalogs would only hide the failure behind an unchanged UI.
    expect(spy.mock.calls.filter((c) => c[0] === '/api/i18n').length).toBe(before)
  })
})

describe('ConfigView — logs', () => {
  beforeEach(resetMocks)

  it('no longer carries the recent errors card', async () => {
    // Moved to the System tab, where the page refreshes itself: a frozen list
    // of errors in the middle of settings is never re-read. Checked here, and
    // not only in SystemView, so that a rollback would be visible.
    const { w } = await mountView({
      '/api/logs': { lines: ['WARN plugin radio unavailable'] },
    })
    expect(w.findAll('[data-log-line]')).toHaveLength(0)
    expect(w.text()).not.toContain('Dernières erreurs')
    // The table of contents no longer has an entry pointing at nothing.
    expect(w.findAll('[data-toc-link]').map((l) => l.text())).not.toContain('Dernières erreurs')
  })
})

describe('ConfigView — audio output', () => {
  beforeEach(resetMocks)

  it('sends the PUT of the chosen device, unchanged', async () => {
    const { w, puts } = await mountView()
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=HDMI')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', body: { device: 'hw:CARD=HDMI' } }])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('without a saved choice, the default entry is selected and "Change" sends null', async () => {
    // No more fallback to the first device: `current: null` is a legitimate
    // state ("follow the system default"), the synthetic entry carries it.
    const { w, puts } = await mountView({
      '/api/audio-output': {
        devices: [{ name: 'hw:CARD=Headphones', description: '' }],
        current: null,
      },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('__system_default__')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', body: { device: null } }])
  })

  it('the default entry is the first of the list', async () => {
    const { w } = await mountView()
    const first = w.findAllComponents(SelectItem)[0]!
    expect(first.attributes('data-audio-default')).toBeDefined()
    expect(first.text()).toBe('Par défaut (système)')
  })

  it('shows the description as primary and the technical name as secondary', async () => {
    const { w } = await mountView()
    const items = w.findAllComponents(SelectItem)
    const withDescription = items.find((i) => i.text().includes('bcm2835 Headphones'))!
    expect(withDescription.text()).toContain('hw:CARD=Headphones')
    // Without a description: the name alone, no empty secondary line.
    const withoutDescription = items.find((i) => i.props('value') === 'hw:CARD=HDMI')!
    expect(withoutDescription.text()).toBe('hw:CARD=HDMI')
  })

  it('a chosen device missing from the list stays visible', async () => {
    // Unplugged card: the current selection is appended at the end of the list
    // (name alone) rather than leaving an empty trigger.
    const { w } = await mountView({
      '/api/audio-output': {
        devices: [{ name: 'hw:CARD=Headphones', description: '' }],
        current: 'hw:CARD=USB',
      },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=USB')
    const values = w.findAllComponents(SelectItem).map((i) => i.props('value'))
    expect(values).toContain('hw:CARD=USB')
  })

  it('no device listed: the default entry remains usable', async () => {
    const { w, puts } = await mountView({ '/api/audio-output': { devices: [], current: null } })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('__system_default__')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', body: { device: null } }])
  })

  it('an unreachable /api/audio-output disables "Change"', async () => {
    // Without this, the selector would show "Par défaut (système)" as if it
    // were the real state, and "Change" would send device: null — a silent
    // reset.
    const { w } = await mountView({ '/api/audio-output': undefined })
    expect(w.find('[data-audio-change]').attributes('disabled')).toBeDefined()
  })
})

describe('ConfigView — settings', () => {
  beforeEach(resetMocks)

  it('shows the settings read from /api/settings', async () => {
    const { w } = await mountView({
      '/api/settings': {
        volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, startup_power: 'standby',
        overlay_ms: 5000, tens_window_ms: 5000,
      },
    })
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('800')
    expect((w.find('[data-hold-interval]').element as HTMLInputElement).value).toBe('250')
    // The startup selector reflects standby.
    const startup = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'standby')
    expect(startup).toBeDefined()
  })

  it('offers "previous state" next to "on" and "standby"', async () => {
    // The three wire values, not only the labels: it is `value` that the PUT
    // sends to the core.
    const { w } = await mountView()
    const startup = w
      .findAllComponents(SelectItem)
      .filter((i) => ['on', 'standby', 'previous'].includes(String(i.props('value'))))
    expect(startup.map((i) => String(i.props('value')))).toEqual(['on', 'standby', 'previous'])
    expect(startup.map((i) => i.text())).toEqual(['allumé', 'veille', 'état précédent'])
  })

  it('saves "previous state" through a PUT of the whole block', async () => {
    const { w, puts } = await mountView()
    const startup = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'on')!
    await startup.vm.$emit('update:modelValue', 'previous')
    await w.find('[data-startup-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        body: {
          volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, startup_power: 'previous',
          overlay_ms: 5000, tens_window_ms: 5000, seek_step_s: 10,
          cover_cache_budget_mio: 50, cover_download_max_mio: 2,
          cover_source_max_mio: 20, cover_max_edge_px: 640, cover_jpeg_quality: 85,
          cover_passthrough_max_ko: 150, cover_max_pixels_mpx: 16, cover_rendition: true,
        },
      },
    ])
  })

  it('saves startup in standby through a PUT of the whole block', async () => {
    const { w, puts } = await mountView()
    const startup = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'on')!
    await startup.vm.$emit('update:modelValue', 'standby')
    await w.find('[data-startup-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        body: {
          volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, startup_power: 'standby',
          overlay_ms: 5000, tens_window_ms: 5000, seek_step_s: 10,
          cover_cache_budget_mio: 50, cover_download_max_mio: 2,
          cover_source_max_mio: 20, cover_max_edge_px: 640, cover_jpeg_quality: 85,
          cover_passthrough_max_ko: 150, cover_max_pixels_mpx: 16, cover_rendition: true,
        },
      },
    ])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('saves the volume-hold delays as numbers', async () => {
    const { w, puts } = await mountView()
    await w.find('[data-hold-initial]').setValue('1500')
    await w.find('[data-hold-interval]').setValue('300')
    await w.find('[data-hold-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        body: {
          volume_repeat_initial_ms: 1500, volume_repeat_interval_ms: 300, startup_power: 'on',
          overlay_ms: 5000, tens_window_ms: 5000, seek_step_s: 10,
          cover_cache_budget_mio: 50, cover_download_max_mio: 2,
          cover_source_max_mio: 20, cover_max_edge_px: 640, cover_jpeg_quality: 85,
          cover_passthrough_max_ko: 150, cover_max_pixels_mpx: 16, cover_rendition: true,
        },
      },
    ])
  })

  it('a refused settings PUT is reported by a toast', async () => {
    const { w } = await mountView({}, 'initial delay out of bounds (200-5000 ms)')
    await w.find('[data-hold-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('initial delay out of bounds (200-5000 ms)')
  })

  it('an unreachable /api/settings leaves the default values', async () => {
    const { w } = await mountView({ '/api/settings': undefined })
    // Same value as the `Default` of `Settings` on the core side (state.rs): the
    // two fallbacks must stay aligned, otherwise the page briefly shows
    // something other than what the device applies.
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('800')
  })
})

describe('ConfigView — overlays', () => {
  beforeEach(resetMocks)

  it('shows the two durations read from /api/settings', async () => {
    const { w } = await mountView({
      '/api/settings': {
        volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, startup_power: 'on',
        overlay_ms: 3000, tens_window_ms: 9000,
      },
    })
    expect((w.find('[data-overlay-ms]').element as HTMLInputElement).value).toBe('3000')
    expect((w.find('[data-tens-window-ms]').element as HTMLInputElement).value).toBe('9000')
  })

  it('saves the two durations as numbers, as a whole block', async () => {
    const { w, puts } = await mountView()
    await w.find('[data-overlay-ms]').setValue('2000')
    await w.find('[data-tens-window-ms]').setValue('7000')
    await w.find('[data-overlays-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        body: {
          volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, startup_power: 'on',
          overlay_ms: 2000, tens_window_ms: 7000, seek_step_s: 10,
          cover_cache_budget_mio: 50, cover_download_max_mio: 2,
          cover_source_max_mio: 20, cover_max_edge_px: 640, cover_jpeg_quality: 85,
          cover_passthrough_max_ko: 150, cover_max_pixels_mpx: 16, cover_rendition: true,
        },
      },
    ])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('an out-of-bounds PUT is reported by a toast', async () => {
    const { w } = await mountView({}, 'overlay out of bounds (1000-15000 ms)')
    await w.find('[data-overlays-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('overlay out of bounds (1000-15000 ms)')
  })
})

describe('ConfigView — seek', () => {
  beforeEach(resetMocks)

  it('sends the seek step', async () => {
    const { w, puts } = await mountView()
    await w.find('[data-seek-step-s]').setValue('30')
    await w.find('[data-seek-change]').trigger('click')
    await flushPromises()
    const sentBody = puts[0]!.body as { seek_step_s: number }
    expect(sentBody.seek_step_s).toBe(30)
  })
})

describe('ConfigView — state of a starting plugin', () => {
  beforeEach(resetMocks)

  it('says "starting" and not "stalled" for a plugin that was just re-enabled', async () => {
    // The defect reported in use: "stalled" means faulty, and showing it during
    // a normal startup accuses a perfectly healthy binary.
    const { w } = await mountView({
      '/api/status': {
        plugins: [{ name: 'mpd', kind: 'unknown', connected: false, admin: false, starting: true }],
        active_source: 'radio',
      } as unknown,
    })
    expect(w.find('[data-plugin-state]').text()).toBe('démarrage')
  })

  it('says "stalled" once the delay has passed', async () => {
    // The control: without it, "always starting" would pass too.
    const { w } = await mountView({
      '/api/status': {
        plugins: [{ name: 'mpd', kind: 'unknown', connected: false, admin: false, stalled: true }],
        active_source: 'radio',
      } as unknown,
    })
    expect(w.find('[data-plugin-state]').text()).toBe('figé')
  })
})

describe('ConfigView — covers', () => {
  beforeEach(resetMocks)

  it('the source cap is never greyed out, the switch does not touch it', async () => {
    // The layout carries a real distinction: this cap applies whether the
    // re-encoding is active or not, and it is the only guard left when the
    // switch is unchecked. Greying it out with the others would be the most
    // costly lie of this card.
    const { w } = await mountView()
    expect(w.find('[data-cover-source-max]').attributes('disabled')).toBeUndefined()

    await w.find('[data-cover-rendition]').trigger('click')
    await flushPromises()
    expect(w.find('[data-cover-source-max]').attributes('disabled')).toBeUndefined()
  })

  it('unchecking the switch greys out the four rendition settings', async () => {
    const { w } = await mountView()
    const fields = [
      '[data-cover-max-edge]',
      '[data-cover-jpeg-quality]',
      '[data-cover-passthrough-max]',
      '[data-cover-max-pixels]',
    ]
    for (const c of fields) expect(w.find(c).attributes('disabled')).toBeUndefined()

    await w.find('[data-cover-rendition]').trigger('click')
    await flushPromises()
    for (const c of fields) {
      expect(w.find(c).attributes('disabled')).toBeDefined()
    }
    // The whole group is announced inactive, once, rather than field by field:
    // that is what a screen reader must hear.
    expect(w.find('[data-cover-rendition-group]').attributes('aria-disabled')).toBe('true')
  })

  it('a greyed-out setting keeps its value and goes back in the PUT', async () => {
    // **Greyed out, not emptied.** Without this property, unchecking then
    // saving would drop the four fields back to the core defaults (the struct
    // is `serde(default)`), i.e. silently lose a setting still shown on
    // screen — and re-checking the switch would not find what had been set.
    const { w, puts } = await mountView()
    await w.find('[data-cover-max-edge]').setValue('800')
    await w.find('[data-cover-rendition]').trigger('click')
    await flushPromises()

    await w.find('[data-cover-change]').trigger('click')
    await flushPromises()
    const body = puts[0]!.body as { cover_rendition: boolean; cover_max_edge_px: number }
    expect(body.cover_rendition).toBe(false)
    expect(body.cover_max_edge_px).toBe(800)
  })

  it('sends the six settings as numbers, never as strings', async () => {
    // Vue's `<input type="number">` yields **strings**: without the
    // `Number(...)` calls in `saveSettings`, the core would receive `"800"` and
    // refuse the whole block with a message about a field the user never
    // touched.
    const { w, puts } = await mountView()
    await w.find('[data-cover-source-max]').setValue('12')
    await w.find('[data-cover-max-edge]').setValue('800')
    await w.find('[data-cover-jpeg-quality]').setValue('70')
    await w.find('[data-cover-passthrough-max]').setValue('256')
    await w.find('[data-cover-max-pixels]').setValue('24')
    await w.find('[data-cover-change]').trigger('click')
    await flushPromises()
    expect(puts[0]!.body).toMatchObject({
      cover_source_max_mio: 12,
      cover_max_edge_px: 800,
      cover_jpeg_quality: 70,
      cover_passthrough_max_ko: 256,
      cover_max_pixels_mpx: 24,
    })
  })

  it('sends the memory budget and the download cap as numbers, never as strings', async () => {
    // Same defect as above (IMPORTANT 3 of the review), on the two fields
    // this task adds: `Input`'s native `v-model` has no `.number` modifier,
    // so an edited field is a string here. Uncast, it fails the core's `u32`
    // deserialization and refuses the *whole* PUT, not just these two
    // fields — the first time a user touches either box.
    const { w, puts } = await mountView()
    await w.find('[data-cover-cache-budget]').setValue('64')
    await w.find('[data-cover-download-max]').setValue('5')
    await w.find('[data-cover-change]').trigger('click')
    await flushPromises()
    expect(puts[0]!.body).toMatchObject({
      cover_cache_budget_mio: 64,
      cover_download_max_mio: 5,
    })
  })

  it('estimates a floor and a typical count from the budget', async () => {
    // Defaults: 50 MiB budget, 2 MiB download cap, 150 KiB pass-through
    // threshold, 640 px / q85 (98 KiB predicted). The entry cost is the max
    // of the predicted weight and the threshold, i.e. 150 KiB: a cover light
    // enough to pass untouched can still weigh up to the threshold.
    // floor = 50 MiB / (2 MiB + 150 KiB) = 23 (worst case: every cover from
    // the internet, paying its download and its entry cost).
    // typical = 50 MiB / 150 KiB = 341, clamped to 256 by MAX_CACHE_ENTRIES:
    // at the product's own defaults, the byte budget stops being the
    // limiting factor and the entry-count belt takes over.
    const { w } = await mountView()
    expect(w.find('[data-cover-cache-estimate]').text()).toBe(
      "Avec 50 Mio de budget, un plafond de 2 Mio et des entrées d'au plus 150 Kio : " +
        'au moins 23 pochettes, environ 256 pour une bibliothèque locale.',
    )
  })

  it('shows the predicted weight of a thumbnail, from the edge and the quality', async () => {
    // The figure the owner could never work out from the two settings alone.
    // Named production change this guards: wiring the line to the threshold
    // (what the removed ceiling used to do) instead of the weight model.
    const { w } = await mountView()
    expect(w.find('[data-cover-predicted-weight]').text()).toContain('98')

    await w.find('[data-cover-max-edge]').setValue('320')
    await flushPromises()
    // 320 px is a quarter of the pixels of 640 px, hence a quarter of the
    // weight: 98 / 4 = 24.5, which rounds up to 25.
    expect(w.find('[data-cover-predicted-weight]').text()).toContain('25')
  })

  it('hides the predicted weight rather than announcing zero', async () => {
    // Clearing a box to retype it is an ordinary keystroke, and `Number('')`
    // is `0`. "A thumbnail weighs about 0 KiB" would be false and alarming.
    const { w } = await mountView()
    await w.find('[data-cover-max-edge]').setValue('')
    await flushPromises()
    expect(w.find('[data-cover-predicted-weight]').exists()).toBe(false)
  })

  it('divides the budget by the entry cost, not by the predicted weight', async () => {
    // The arithmetic lie this test forbids: announcing a 98 KiB predicted
    // weight and then dividing 50 MiB by 98 KiB, which would read 522 --
    // when a cover light enough to pass untouched can weigh up to the
    // 150 KiB threshold.
    //
    // And the expected figure is 256, not 341: 51200 / 150 = 341, but
    // `coverTypicalEstimate` takes the `min` with `MAX_CACHE_ENTRIES`. At the
    // product's own defaults the budget stops being the limiting factor and
    // the belt does. Expecting 341 here would be expecting that `min` to
    // disappear, which would make the estimate dishonest.
    const { w } = await mountView()
    const text = w.find('[data-cover-cache-estimate]').text()
    expect(text).toContain('256')
    expect(text).not.toContain('522')
  })

  it('names its inputs in the estimate, so nothing has to be guessed', async () => {
    // The direct answer to "what influences this".
    const { w } = await mountView()
    const text = w.find('[data-cover-cache-estimate]').text()
    expect(text).toContain('50') // budget
    expect(text).toContain('2') // download ceiling
    expect(text).toContain('150') // cost of one entry
  })

  it('follows the quality into the estimate, which the old ceiling never did', async () => {
    // Under the removed ceiling, changing the quality never moved the
    // estimate an inch: it divided by a declared ceiling. This is the defect
    // this test keeps from coming back.
    const { w } = await mountView()
    const before = w.find('[data-cover-cache-estimate]').text()
    await w.find('[data-cover-max-edge]').setValue('1024')
    await flushPromises()
    expect(w.find('[data-cover-cache-estimate]').text()).not.toBe(before)
  })

  it('drops the per-cover figure when re-encoding is off', async () => {
    // Re-encoding off means no thumbnail is produced at all: a local cover's
    // cost falls to zero, so the "typical" formula would divide by zero.
    // Printing "about 256" (the core's internal MAX_CACHE_ENTRIES, which
    // this case would otherwise reduce to) would expose a constant the user
    // has no way to interpret. The floor is unaffected by the switch — a
    // network cover with no rendition still costs its downloaded bytes —
    // and becomes 50 / 2 = 25 now that the thumbnail no longer adds to the
    // per-entry cost.
    const { w } = await mountView()
    await w.find('[data-cover-rendition]').trigger('click')
    await flushPromises()
    const text = w.find('[data-cover-cache-estimate]').text()
    expect(text).not.toContain('256')
    expect(text).toBe('Au moins 25 pochettes ; le cache en garde quelques centaines au plus.')
  })

  it('keeps the two-figure wording while the pass-through threshold is being retyped', async () => {
    // The ceiling this test named is gone; the box being retyped mid-keystroke
    // is now the pass-through threshold. Clearing a number input to retype it
    // is an ordinary keystroke, and it used to make `Number('') || 0` collapse
    // to zero — the same value the switch being off produces. The page then
    // announced "re-encoding is off" with the switch visibly on, which is
    // simply a false statement about the appliance.
    //
    // Named production change this guards: testing `coverEntryBytes <= 0`
    // instead of `settings.cover_rendition` to choose the sentence.
    const { w } = await mountView()
    await w.find('[data-cover-passthrough-max]').setValue('')
    await flushPromises()
    const text = w.find('[data-cover-cache-estimate]').text()
    expect(text).toContain('pour une bibliothèque locale')
    expect(text).not.toContain('quelques centaines au plus')
  })

  it('never announces a floor of zero covers', async () => {
    // 8 MiB of budget, a 20 MiB download cap and a 2048 KiB pass-through
    // threshold (the field's own ceiling) floor to `Math.floor(8 / 22)` = 0,
    // and the page read "at least 0 covers" — alarming and false.
    // `evict_to_budget` protects the entry its caller just inserted, so one
    // cover is what the core actually guarantees however badly the boxes are
    // set.
    //
    // Named production change this guards: dropping the `Math.max(1, …)` in
    // `coverFloorEstimate`.
    const { w } = await mountView()
    await w.find('[data-cover-cache-budget]').setValue('8')
    await w.find('[data-cover-download-max]').setValue('20')
    await w.find('[data-cover-passthrough-max]').setValue('2048')
    await flushPromises()
    expect(w.find('[data-cover-cache-estimate]').text()).toContain('au moins 1 pochettes')
  })
})

describe('ConfigView — table of contents', () => {
  beforeEach(resetMocks)

  it('lists one entry per section, with the label of its card', async () => {
    const { w } = await mountView()
    const links = w.findAll('[data-toc-link]')
    // No more "Dernières erreurs": the card moved to the System tab, and the
    // table of contents must not keep an entry pointing at nothing.
    expect(links.map((l) => l.text())).toEqual([
      'Plugins', 'Sortie audio', 'Langue', 'Démarrage', 'Date et heure', 'Volume maintenu',
      'Incrustations', 'Déplacement', "Pochettes d'album",
    ])
    // Hidden on small screens: the column follows the shell width, there is no
    // room for it on mobile.
    expect(w.find('[data-toc]').classes()).toContain('hidden')
  })

  it('a click smoothly scrolls to the section and marks it active', async () => {
    const { w } = await mountView()
    const scrollIntoView = vi.fn()
    const target = w.find('#audio')
    expect(target.exists()).toBe(true)
    target.element.scrollIntoView = scrollIntoView
    await w.findAll('[data-toc-link]')[1]!.trigger('click')
    expect(scrollIntoView).toHaveBeenCalledWith({ behavior: 'smooth' })
    expect(w.findAll('[data-toc-link]')[1]!.attributes('aria-current')).toBe('true')
  })

  it('scrolling updates the active section (scrollspy)', async () => {
    const { w } = await mountView()
    expect(ioCallback).not.toBeNull()
    ioCallback!([{ target: w.find('#language').element, isIntersecting: true }])
    ioCallback!([{ target: w.find('#plugins').element, isIntersecting: false }])
    await w.vm.$nextTick()
    const activeLinks = w.findAll('[data-toc-link][aria-current="true"]')
    expect(activeLinks).toHaveLength(1)
    expect(activeLinks[0]!.text()).toBe('Langue')
  })
})
