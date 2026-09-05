import { toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import CdAdmin from './CdAdmin.vue'

// Same approach as `MpdAdmin.test.ts`: keep the real module (components,
// `api`, ...) and replace only the two `toast` entries this view uses, so
// they can be observed without showing a notification.
vi.mock('@ritornello/ui', async () => {
  const real = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...real, toast: { ...real.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOG = {
  title: 'CD audio',
  arrival_label: "À l'arrivée sur cette source",
  arrival_help: "S'applique à la touche CD comme au démarrage.",
  arrival_nothing: 'Ne rien lancer',
  arrival_first_track: 'Lancer la piste 1',
  arrival_last_track: 'Reprendre la dernière piste écoutée',
  arrival_last_track_help: 'Uniquement sur le même disque.',
  btn_save: 'Enregistrer',
  saved: 'Enregistré',
  save_failed: "l'enregistrement a échoué",
  bad_request: 'requête invalide : {detail}',
}

// Absolute prefix the shell passes through the (required) `base` prop: that is
// the contract, this view does not know the name under which it is served.
const BASE = '/plugins/cd/'

/** Mounts the component with a spied `fetch` serving `on_arrival` on GET. */
async function mountView(on_arrival = 'nothing') {
  const puts: Array<{ url: string; body: unknown }> = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      puts.push({ url, body: JSON.parse(String(init.body)) })
      return new Response(null, { status: 204 })
    }
    return new Response(JSON.stringify({ on_arrival }), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(CdAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, spy, puts }
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
})

describe('CdAdmin', () => {
  it('shows the setting received from the server, by its label', async () => {
    const { w } = await mountView('first_track')
    expect(w.text()).toContain(CATALOG.title)
    expect(w.text()).toContain(CATALOG.arrival_label)
    // The trigger displays the label, never the raw value: `first_track` on
    // screen would be a bug, not a shorthand.
    const trigger = w.get('[data-arrival]')
    expect(trigger.text()).toContain(CATALOG.arrival_first_track)
    expect(trigger.text()).not.toContain('first_track')
  })

  it('shows the label even when the catalog arrives after mounting', async () => {
    // The bug the owner hit: the trigger showed `arrival_nothing`, the raw
    // key, while the rest of the page was correctly translated.
    //
    // `PluginView` mounts a plugin's component **before** its catalog has
    // arrived, on purpose — hidden behind the skeleton, so the component's
    // own request leaves during the wait instead of after it. So the first
    // render really does happen with an empty catalog, and `createT` then
    // falls back on the key. Every other label recovers on the re-render;
    // the one inside the Select did not, because the kit's `SelectItemText`
    // registers an item's text with the Select root when the item mounts and
    // never re-reads it.
    //
    // Mounting with `{}` then handing over the real catalog is exactly what
    // the shell does, and it is the only way this test can see the defect —
    // the previous ones all mounted with a complete catalog.
    const spy = vi.fn(async () => new Response(JSON.stringify({ on_arrival: 'nothing' }), { status: 200 }))
    vi.stubGlobal('fetch', spy)
    const w = mount(CdAdmin, { props: { catalog: {}, base: BASE } })
    await flushPromises()
    await w.setProps({ catalog: CATALOG })
    await flushPromises()
    const trigger = w.get('[data-arrival]')
    expect(trigger.text()).toContain(CATALOG.arrival_nothing)
    expect(trigger.text()).not.toContain('arrival_nothing')
  })

  it('names the control for a screen reader', async () => {
    // The kit's `Select` does not associate a neighbouring `<label for>` with
    // its trigger, so without this the control has no accessible name at all.
    const { w } = await mountView()
    expect(w.get('[data-arrival]').attributes('aria-label')).toBe(CATALOG.arrival_label)
  })

  it('reads the setting from the URL the shell provides, never a guessed one', async () => {
    // The name under which the plugin is served comes from the deployment: a
    // hardcoded `/plugins/cd/` would silently query a nonexistent plugin.
    const { spy } = await mountView()
    // The URL alone, not the whole call: `api.get` passes no second argument
    // to `fetch`, so matching on one would be asserting the kit's internals.
    expect(spy.mock.calls[0]?.[0]).toBe(`${BASE}api/data`)
  })

  it('sends the chosen value and confirms the save', async () => {
    const { w, puts } = await mountView('nothing')
    await w.get('[data-save]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: `${BASE}api/data`, body: { on_arrival: 'nothing' } }])
    expect(toast.success).toHaveBeenCalledWith(CATALOG.saved)
    expect(toast.error).not.toHaveBeenCalled()
  })

  it('explains the same-disc rule only when the resume is selected', async () => {
    // The rule applies to one choice out of three. Showing it always would
    // have the page explain something that is not in force.
    const { w } = await mountView('last_track')
    expect(w.find('[data-resume-help]').exists()).toBe(true)
    expect(w.get('[data-resume-help]').text()).toBe(CATALOG.arrival_last_track_help)

    const { w: other } = await mountView('first_track')
    expect(other.find('[data-resume-help]').exists()).toBe(false)
  })

  it('always explains that the setting covers both ways of arriving', async () => {
    // The whole point of the setting: one value for the source key and for
    // the boot. A page that did not say so would leave the owner guessing
    // which of the two it governs.
    const { w } = await mountView()
    expect(w.get('[data-arrival-help]').text()).toBe(CATALOG.arrival_help)
  })

  it('falls back on the default when the server sends a value it does not know', async () => {
    // An older plugin, or a hand-edited state file. Assigned blindly, the
    // `Select` would point at a value with no matching item — a blank
    // control. The default at least shows what the plugin really does.
    const { w } = await mountView('eject_and_run')
    expect(w.get('[data-arrival]').text()).toContain(CATALOG.arrival_nothing)
  })

  it('shows the server refusal as it stands, without retranslating it', async () => {
    // The server resolves its own catalog keys (same convention as the other
    // plugins): re-translating here would need a second copy of the rule, and
    // showing the bare key would put `bad_request` on screen.
    const refusal = 'requête invalide : ceci est le texte du serveur'
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        return new Response(JSON.stringify({ error: refusal }), { status: 422 })
      }
      return new Response(JSON.stringify({ on_arrival: 'nothing' }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(CdAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    await w.get('[data-save]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith(refusal)
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('reports a failed load instead of showing an empty control', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      throw new Error('network down')
    }))
    const w = mount(CdAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(toast.error).toHaveBeenCalled()
    // And the control still shows the default rather than nothing at all.
    expect(w.get('[data-arrival]').text()).toContain(CATALOG.arrival_nothing)
  })
})
