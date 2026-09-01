import { toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import MpdAdmin from './MpdAdmin.vue'

// Same approach as `ConfigView.test.ts`: keep the real module (components,
// `api`, ...) and replace only the two `toast` entries this view uses, so
// they can be observed without showing a notification.
vi.mock('@ritornello/ui', async () => {
  const real = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...real, toast: { ...real.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOG = {
  title: 'Serveur MPD',
  listen_label: "Adresse d'écoute",
  port_label: 'Port',
  restart_notice: 'Le changement ne prend effet quau redémarrage du greffon.',
  btn_save: 'Enregistrer',
  saved: 'Enregistré',
  listen_empty: "L'adresse d'écoute ne peut pas être vide.",
  port_zero: 'Le port doit être compris entre 1 et 65535.',
  save_failed: "l'enregistrement a échoué",
  bad_request: 'requête invalide : {detail}',
}

// Absolute prefix the shell passes through the (required) `base` prop: that is
// the contract, this view does not know the name under which it is served.
const BASE = '/plugins/mpd/'

/** Mounts the component with a spied `fetch` serving `data` on GET. */
async function mountView(data: { listen: string; port: number } = { listen: '0.0.0.0', port: 6600 }) {
  const puts: Array<{ url: string; body: unknown }> = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      puts.push({ url, body: JSON.parse(String(init.body)) })
      return new Response(null, { status: 204 })
    }
    return new Response(JSON.stringify(data), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(MpdAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, spy, puts }
}

/** Variant where the PUT is refused by the server (422), like a real validation refusal. */
async function mountWithRefusal(error: string) {
  const data = { listen: '0.0.0.0', port: 6600 }
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      return new Response(JSON.stringify({ error }), { status: 422 })
    }
    return new Response(JSON.stringify(data), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(MpdAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, spy }
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
})

describe('MpdAdmin', () => {
  it('shows both fields with their labels and the values received from the server', async () => {
    const { w } = await mountView({ listen: '192.168.1.10', port: 6601 })

    // The catalog labels do appear in the template (and not a raw key, which
    // would indicate a missing entry or a wrong import).
    expect(w.text()).toContain("Adresse d'écoute")
    expect(w.text()).toContain('Port')

    const listenField = w.get('[data-listen]').element as HTMLInputElement
    const portField = w.get('[data-port]').element as HTMLInputElement
    // Proof that the value does come from the GET response, not from the
    // internal defaults (`0.0.0.0` / `6600`): a test that left the default
    // values in place would pass even if the GET were never consumed.
    expect(listenField.value).toBe('192.168.1.10')
    expect(portField.value).toBe('6601')
  })

  it('sends the edited values on save', async () => {
    const { w, puts } = await mountView({ listen: '0.0.0.0', port: 6600 })

    await w.get('[data-listen]').setValue('10.0.0.5')
    await w.get('[data-port]').setValue('6601')
    await w.get('[data-save]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.url).toBe('/plugins/mpd/api/data')
    // The port must be a number in the sent body, not the string typed into
    // the field: `Config` (Rust side) expects a JSON integer.
    expect(puts[0]!.body).toEqual({ listen: '10.0.0.5', port: 6601 })
    expect(toast.success).toHaveBeenCalledWith('Enregistré')
  })

  // The client-side guard (`hasErrors`) is modelled exactly on
  // `Config::validate`: a non-empty address and a port in 1..=65535 are the
  // only two refusals the server knows for these two fields, so a pair that
  // passes the guard is by construction accepted on the server side -- there
  // is no value the client would let through that the server would refuse
  // *for these two reasons*. This test therefore exercises a refusal that has
  // nothing to do with the shape of the fields: `save_failed`, a disk write
  // failure (I/O), which no client-side guard can anticipate. It is the only
  // 422 refusal still reachable with valid input.
  it('a 422 refusal (disk failure, not detectable client-side) shows the server-translated message', async () => {
    const { w } = await mountWithRefusal("l'enregistrement a échoué")

    // Input perfectly valid according to the client guard: the refusal
    // therefore does come from the server, not from a local block.
    expect((w.get('[data-save]').element as HTMLButtonElement).disabled).toBe(false)
    await w.get('[data-save]').trigger('click')
    await flushPromises()

    // The displayed message is exactly the text returned by the server
    // (already resolved from the catalog key on the Rust side): neither a JS
    // exception nor a raw key (`save_failed`).
    expect(toast.error).toHaveBeenCalledWith("l'enregistrement a échoué")
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('a port at 0 marks the field invalid and disables Save: no PUT leaves', async () => {
    const { w, spy } = await mountView({ listen: '0.0.0.0', port: 6600 })

    await w.get('[data-port]').setValue('0')

    const portField = w.get('[data-port]')
    const button = w.get('[data-save]').element as HTMLButtonElement
    expect(portField.attributes('aria-invalid')).toBe('true')
    expect(button.disabled).toBe(true)
    expect(w.find('[data-port-error]').exists()).toBe(true)

    // `dispatchEvent` rather than VTU's `trigger()`: the latter gives up on
    // its own on a `disabled` element, which would make this test pass
    // without any guard being exercised in the view's code (see the same
    // choice in `RadioAdmin.test.ts`). So the click is dispatched directly:
    // what is tested here is the early return of `save()`, not merely the
    // button's visual state.
    button.dispatchEvent(new Event('click'))
    await flushPromises()

    expect(spy.mock.calls.some((c) => (c[1] as RequestInit | undefined)?.method === 'PUT')).toBe(false)
    expect(toast.error).not.toHaveBeenCalled()
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('an empty address marks the field invalid and disables Save', async () => {
    const { w } = await mountView({ listen: '0.0.0.0', port: 6600 })

    await w.get('[data-listen]').setValue('   ')

    expect(w.get('[data-listen]').attributes('aria-invalid')).toBe('true')
    expect((w.get('[data-save]').element as HTMLButtonElement).disabled).toBe(true)
    expect(w.find('[data-listen-error]').exists()).toBe(true)
  })

  it('warns about the required restart as soon as it loads, without waiting for a save', async () => {
    const { w } = await mountView()

    // The notice is present immediately: the port does not change on the fly,
    // so reading it before acting must be possible without having clicked
    // Save. A test that only checked this after a click on Save would miss the
    // regression this case encodes.
    const notice = w.get('[data-restart-notice]')
    expect(notice.text()).toBe('Le changement ne prend effet quau redémarrage du greffon.')
    expect(toast.success).not.toHaveBeenCalled()
    expect(toast.error).not.toHaveBeenCalled()
  })
})
