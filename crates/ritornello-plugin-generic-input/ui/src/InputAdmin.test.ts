import { Select } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import InputAdmin from './InputAdmin.vue'

const CATALOG = {
  device_label: 'Périphérique', btn_refresh: 'Rafraîchir', col_action: 'Action', col_code: 'Code',
  btn_learn: 'Apprendre', btn_clear: 'Effacer', btn_save: 'Enregistrer', btn_cancel: 'Annuler',
  btn_load_preset: 'Charger', btn_import: 'Importer', btn_export: 'Exporter',
  preset_label: 'Preset', learn_timeout: 'Délai dépassé',
  dlg_learn_title: 'Apprentissage d’une touche',
  dlg_learn_desc: 'Appuyez sur une touche du périphérique « {device} »…',
  learn_append_label: 'Ajouter aux codes existants',
  learn_countdown: 'Il reste {s} s',
  saved: 'Enregistré', save_error: 'Échec : ', load_error: 'Erreur : ', no_device: 'Aucun périphérique',
  conflict_code: 'le code {code} est déjà affecté à {action}',
  conflict_dup: 'le code {code} est saisi deux fois',
  save_conflicts: 'Corrigez les codes en double avant d’enregistrer',
  act_mute: 'Muet', act_power: 'Veille',
}

const DATA = {
  devices: ['mce', 'keyboard'],
  bindings: { devices: [{ name: 'mce', bindings: [{ code: 9, cmd: 'Mute' }] }] },
  presets: ['mce', 'keyboard'],
  learning: null as { captured: number | null } | null,
}

// Absolute prefix the shell passes via the (required) `base` prop: that's
// the contract, this view does not know the name under which it is served.
const BASE = '/plugins/generic-input/'

const PROBE_MS = 300

function stub(data: () => unknown) {
  const spy = vi.fn(async (_u: string, init?: RequestInit) =>
    init?.method === 'PUT'
      ? new Response(null, { status: 204 })
      : new Response(JSON.stringify(data()), { status: 200 }),
  )
  vi.stubGlobal('fetch', spy)
  return spy
}

// The learning popup is teleported to a portal on `document.body` (like
// every kit `Dialog`): `wrapper.find()` never sees it. Hence the
// `attachTo` on every mount, the lookup in the document, and the cleanup
// of `document.body` between tests (a portal survives its wrapper's
// unmount).
function mountView(base = BASE) {
  return mount(InputAdmin, { props: { catalog: CATALOG, base }, attachTo: document.body })
}

const inPopup = (selector: string) => document.body.querySelector(selector)
const popup = () => inPopup('[data-dlg-learn]')

/** The `op`s of the emitted PUTs, in order. */
const ops = (spy: ReturnType<typeof stub>) =>
  spy.mock.calls
    .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
    .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)

async function mountLoaded(data: () => unknown = () => DATA) {
  const spy = stub(data)
  const w = mountView()
  await flushPromises()
  return { w, spy }
}

async function checkAppend() {
  const checkbox = inPopup('[data-learn-append]') as HTMLInputElement
  checkbox.checked = true
  checkbox.dispatchEvent(new Event('change'))
  await flushPromises()
}

// The row of an action, identified by the label of its **first** cell: a
// `r.text().includes(label)` would also match a row whose conflict message
// names this action.
const actionRow = (w: ReturnType<typeof mountView>, label: string) =>
  w.findAll('[data-action-row]').find((r) => r.findAll('td')[0]!.text() === label)!

// Full learning scenario under fake timers: opens the popup on the row
// carrying `label`, "add" checkbox checked or not, then a code captured by
// the server on the next probe.
async function learnAndCapture(label: string, code: number, add = false) {
  vi.useFakeTimers()
  let captured: number | null = null
  const spy = stub(() => ({ ...DATA, learning: { captured } }))
  const w = mountView()
  await vi.advanceTimersByTimeAsync(0)
  // `actionRow`, not a `text().includes`: as soon as the captured code
  // creates a conflict, the faulty row's message names the other action,
  // and a search over the whole row text would return the wrong one.
  const row = () => actionRow(w, label)
  await row().find('[data-learn]').trigger('click')
  await vi.advanceTimersByTimeAsync(0)
  if (add) await checkAppend()
  captured = code
  await vi.advanceTimersByTimeAsync(PROBE_MS)
  return { w, spy, row, value: () => (row().find('input').element as HTMLInputElement).value }
}

// Healthy table loaded ("Muet" carries code 9), then this same 9 typed by
// hand into "Veille": two rows in conflict, exactly what the server would
// reject at save time (`duplicate_code`).
async function conflictBetweenTwoRows() {
  const { w, spy } = await mountLoaded()
  await actionRow(w, 'Veille').find('input').setValue('9')
  return { w, spy }
}

describe('InputAdmin', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    document.body.innerHTML = ''
  })
  afterEach(() => vi.useRealTimers())

  it('lists the devices, the presets and the 23 actions', async () => {
    const { w } = await mountLoaded()
    expect(w.findAll('[data-action-row]')).toHaveLength(23)
    expect(w.find('[data-device-select]').exists()).toBe(true)
  })

  it('prefills the codes of the selected device', async () => {
    const { w } = await mountLoaded()
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    expect((muteRow.find('input').element as HTMLInputElement).value).toBe('9')
  })

  it('clears a code without touching the server', async () => {
    const { w, spy } = await mountLoaded()
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-clear]').trigger('click')
    expect((muteRow.find('input').element as HTMLInputElement).value).toBe('')
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('saves the current devices complete table', async () => {
    const { w, spy } = await mountLoaded()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')!
    const body = JSON.parse(String((put[1] as RequestInit).body))
    expect(body.op).toBe('save')
    expect(body.bindings.devices.find((d: { name: string }) => d.name === 'mce').bindings).toEqual([
      { code: 9, cmd: 'Mute' },
    ])
  })

  it('learning: probes every 300 ms then fills in the captured code', async () => {
    vi.useFakeTimers()
    let captured: number | null = null
    const spy = stub(() => ({ ...DATA, learning: { captured } }))
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    // The "press a key" instruction and the cancellation now live in the
    // popup, not in the bottom bar.
    expect(popup()!.textContent).toContain('Appuyez sur une touche')
    expect(inPopup('[data-learn-cancel]')).not.toBeNull()
    expect(
      JSON.parse(String((spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')![1] as RequestInit).body)),
    ).toEqual({ op: 'learn', device: 'mce' })
    captured = 42
    await vi.advanceTimersByTimeAsync(PROBE_MS)
    expect((muteRow.find('input').element as HTMLInputElement).value).toBe('42')
    // Probing stops, `cancel_learn` is emitted, and the popup closes.
    expect(ops(spy)).toContain('cancel_learn')
    expect(popup()).toBeNull()
  })

  it('learning: the popup opens and its title names the learned action', async () => {
    vi.useFakeTimers()
    stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    const content = popup()!.textContent!
    expect(content).toContain('Apprentissage d’une touche')
    // The translated label of **this** row, not another.
    expect(content).toContain('Muet')
    expect(content).not.toContain('Veille')
    // And the current device, named by the description.
    expect(content).toContain('mce')
  })

  it('learning: the countdown starts at 30 s and decreases', async () => {
    // Without it, the popup would close itself after 30 s with nothing
    // having warned of the deadline: the user would think the device is
    // silent when they simply took too long to find the key.
    vi.useFakeTimers()
    stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    // Set right at opening, without waiting for the first probing round.
    expect(popup()!.querySelector('[data-learn-countdown]')!.textContent).toContain('30')
    // Ten full rounds, not "3,000 ms": the figure only refreshes at the
    // probing's pace, so an advance that falls between two rounds would
    // leave the previous round's value.
    await vi.advanceTimersByTimeAsync(PROBE_MS * 10)
    expect(popup()!.querySelector('[data-learn-countdown]')!.textContent).toContain('27')
  })

  it('learning: box unchecked, the captured code replaces the field', async () => {
    // "Muet" already carries code 9: without the box checked, the captured
    // code takes its place instead of being added to it.
    const { value } = await learnAndCapture('Muet', 42)
    expect(value()).toBe('42')
    expect(popup()).toBeNull()
  })

  it('learning: box checked, the captured code is appended to the field', async () => {
    const { value } = await learnAndCapture('Muet', 42, true)
    expect(value()).toBe('9, 42')
    expect(popup()).toBeNull()
  })

  it('learning: box checked, a code already present leaves the field intact', async () => {
    // No "9, 9": the server would reject the whole table (`duplicate_code`),
    // and the user hasn't asked for anything more.
    const { value } = await learnAndCapture('Muet', 9, true)
    expect(value()).toBe('9')
  })

  it('learning: box checked on a row with no code, the field holds just the code', async () => {
    // "Veille" carries no code: appending must give "42" and not ", 42".
    // A case specified by `applyCode` (`!field.trim()`) but that the other
    // tests, all on "Muet" (already carrying code 9), did not cover.
    const { value } = await learnAndCapture('Veille', 42, true)
    expect(value()).toBe('42')
  })

  it('learning: the "add" box comes back unchecked on every opening', async () => {
    const { row } = await learnAndCapture('Muet', 42, true)
    expect(popup()).toBeNull()
    await row().find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect((inPopup('[data-learn-append]') as HTMLInputElement).checked).toBe(false)
  })

  it('learning: the popups "Cancel" button cancels the server session', async () => {
    vi.useFakeTimers()
    const spy = stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(ops(spy)).not.toContain('cancel_learn')
    ;(inPopup('[data-learn-cancel]') as HTMLButtonElement).click()
    await vi.advanceTimersByTimeAsync(0)
    expect(ops(spy)).toContain('cancel_learn')
    expect(popup()).toBeNull()
  })

  it('learning: a cancellation failing over the network still closes the popup', async () => {
    // Cancellation is now the standard gesture (button, close icon,
    // Escape, overlay) and its `cancel_learn` PUT can fail. The popup must
    // still close and probing must still die: `stopLearn` does both
    // before any `await`.
    vi.useFakeTimers()
    const spy = vi.fn(async (_u: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        if (JSON.parse(String(init.body)).op === 'cancel_learn') throw new Error('network cut off')
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ ...DATA, learning: { captured: null } }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    await actionRow(w, 'Muet').find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    ;(inPopup('[data-learn-cancel]') as HTMLButtonElement).click()
    await vi.advanceTimersByTimeAsync(0)
    // The popup closes anyway: it does not depend on the round trip.
    expect(popup()).toBeNull()
    // And probing is indeed dead: not a single request, GETs included, in
    // the following second -- a surviving interval would probe every
    // 300 ms. `ops` only keeps the PUTs, it would not see these GETs:
    // hence the raw call count, on top of the absence of a second
    // `cancel_learn`.
    const calls = spy.mock.calls.length
    await vi.advanceTimersByTimeAsync(1_000)
    expect(spy.mock.calls.length).toBe(calls)
    expect(ops(spy)).toEqual(['learn', 'cancel_learn'])
  })

  it('learning: two concurrent triggers on two rows never create two timers', async () => {
    // Mutation explicitly tested (see report, "Fix round 1" section): with
    // the original guard (based solely on `timer`, assigned only after the
    // `learn` PUT's `await`), this test fails -- two `learn` PUTs go out,
    // and the orphaned interval can write the captured code into the
    // wrong row.
    vi.useFakeTimers()
    let unblockLearn: () => void = () => {}
    const learnInProgress = new Promise<void>((r) => (unblockLearn = r))
    let captured: number | null = null
    const spy = vi.fn(async (_u: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        const body = JSON.parse(String(init.body))
        if (body.op === 'learn') await learnInProgress
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ ...DATA, learning: { captured } }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    const standbyRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Veille'))!

    // Two triggers on two different rows before the first `learn` PUT
    // makes its round trip (double-click, or click on another action --
    // plausible on a Pi 2 where the round trip is not instantaneous).
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    await standbyRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)

    // Only one `learn` PUT should have gone out: the synchronous guard
    // must stop the second trigger before any network call.
    const learnPuts = spy.mock.calls.filter(
      (c) => (c[1] as RequestInit)?.method === 'PUT' && JSON.parse(String((c[1] as RequestInit).body)).op === 'learn',
    )
    expect(learnPuts).toHaveLength(1)

    captured = 42
    unblockLearn()
    await vi.advanceTimersByTimeAsync(300)

    // Only the row whose `learn` PUT actually went out ("Muet") receives
    // the captured code; "Veille" -- whose trigger was rejected by the
    // guard -- stays empty.
    expect((muteRow.find('input').element as HTMLInputElement).value).toBe('42')
    expect((standbyRow.find('input').element as HTMLInputElement).value).toBe('')
  })

  it('addresses its requests under the absolute prefix received via the `base` prop', async () => {
    // IMPORTANT 6 from the final review. This view used to call
    // `api.get('./api/data')` relatively, so resolved against the browser's
    // URL and not against anything the contract guarantees: on
    // `/plugins/generic-input` (without a trailing slash, a form the
    // shell's router also accepted), `./api/data` resolved to
    // `/plugins/api/data` — which the core interprets as the "api" plugin:
    // 404, empty table and every button failing.
    const spy = stub(() => DATA)
    // Deliberately a prefix that is **not** `/plugins/generic-input/`: the
    // name under which a plugin is served comes from `plugins.toml`, i.e.
    // from deployment. This test would fail if the view rebuilt its own
    // name instead of honoring the received prefix.
    const w = mountView('/plugins/remote/')
    await flushPromises()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.length).toBeGreaterThan(1)
    for (const call of spy.mock.calls) {
      expect(call[0]).toBe('/plugins/remote/api/data')
    }
  })

  it('learning: changing device cancels the ongoing session', async () => {
    // IMPORTANT 2 from the final review. The old handler used to cancel
    // learning on a device change
    // (`$('dev').onchange = async () => { if (timer) await stopLearn(''); … }`);
    // `watch(device, fillCodes)` had lost this cancellation.
    //
    // Without it, the interval keeps probing while the server's learning
    // session is still armed on the **previous** device, `fillCodes()` has
    // in the meantime repopulated the table from the **new** device's
    // bindings, and the closure writes the captured code into the new
    // device's row -- which "Save" then persists. Same class of bug as the
    // race fixed in Task 12, whose fix had not considered a device change.
    vi.useFakeTimers()
    let captured: number | null = null
    const spy = stub(() => ({ ...DATA, learning: { captured } }))
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)

    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(popup()!.textContent).toContain('Appuyez sur une touche')

    expect(ops(spy)).not.toContain('cancel_learn')

    // Device change: "mce" -> "keyboard".
    await w.findAllComponents(Select)[0]!.vm.$emit('update:modelValue', 'keyboard')
    await vi.advanceTimersByTimeAsync(0)

    // 1. The server session is explicitly cancelled.
    expect(ops(spy)).toContain('cancel_learn')
    // 2. The UI is no longer in the "press a key" state for a device
    //    nobody is learning anymore: the popup, which carries this
    //    sentence and the cancellation, has disappeared.
    expect(popup()).toBeNull()

    // 3. The code the previous device would eventually have captured must
    //    not land in any row of the new device's table.
    captured = 42
    await vi.advanceTimersByTimeAsync(1_000)
    const codes = w
      .findAll('[data-action-row]')
      .map((r) => (r.find('input').element as HTMLInputElement).value)
    expect(codes.every((v) => v === '')).toBe(true)
  })

  it('learning: the table is repopulated even if the network cancellation fails', async () => {
    // Fix folded in (final review): `watch(device, …)` used to do
    // `await stopLearn('')` with no safety net. If `fetch` rejects
    // (network cut off), the uncaught rejection would skip `fillCodes()`,
    // and the **previous** device's codes would stay displayed under the
    // new one -- exactly the class of bug this watcher was meant to fix,
    // in the network-failure branch.
    vi.useFakeTimers()
    const bindings = {
      devices: [
        { name: 'mce', bindings: [{ code: 9, cmd: 'Mute' }] },
        { name: 'keyboard', bindings: [{ code: 5, cmd: 'Mute' }] },
      ],
    }
    const spy = vi.fn(async (_u: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        const body = JSON.parse(String(init.body))
        if (body.op === 'cancel_learn') throw new Error('network cut off')
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ ...DATA, bindings, learning: null }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)

    const muteRow = () => w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    expect((muteRow().find('input').element as HTMLInputElement).value).toBe('9')

    await muteRow().find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(popup()!.textContent).toContain('Appuyez sur une touche')

    // Device change: the cancellation (`cancel_learn`) fails over the
    // network. Without the fix, the "Muet" row would stay at "9" (the
    // "mce" bindings) instead of being repopulated for "keyboard".
    await w.findAllComponents(Select)[0]!.vm.$emit('update:modelValue', 'keyboard')
    await vi.advanceTimersByTimeAsync(0)

    expect((muteRow().find('input').element as HTMLInputElement).value).toBe('5')
  })

  it('learning: gives up after 30 s with the timeout message, not before', async () => {
    vi.useFakeTimers()
    const spy = stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = mountView()
    await vi.advanceTimersByTimeAsync(0)
    const muteRow = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muteRow.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    // At 29 s, the 30 s cap is not yet reached: the popup is still open and
    // nothing has been cancelled. Without this assertion taken before the
    // deadline, the test would not distinguish 30 s from a shorter
    // timeout -- the original 10 s make it fail here, which is the point.
    await vi.advanceTimersByTimeAsync(29_000)
    expect(popup()).not.toBeNull()
    expect(ops(spy)).not.toContain('cancel_learn')
    expect(w.text()).not.toContain('Délai dépassé')
    // At 31 s, the deadline is crossed: the popup closes and the timeout
    // message displays in the bottom bar, now visible.
    await vi.advanceTimersByTimeAsync(2_000)
    expect(popup()).toBeNull()
    expect(ops(spy)).toContain('cancel_learn')
    expect(w.text()).toContain('Délai dépassé')
  })

  it('with no device, warns and emits no operation (save)', async () => {
    const { w, spy } = await mountLoaded(() => ({ ...DATA, devices: [] }))
    expect(w.text()).toContain('Aucun périphérique')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('with no device, warns and emits no operation (learn)', async () => {
    const { w, spy } = await mountLoaded(() => ({ ...DATA, devices: [] }))
    const oneRow = w.findAll('[data-action-row]')[0]!
    await oneRow.find('[data-learn]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('with no device, warns and emits no operation (load_preset)', async () => {
    const { w, spy } = await mountLoaded(() => ({ ...DATA, devices: [] }))
    const button = w.findAll('button').find((b) => b.text() === CATALOG.btn_load_preset)!
    await button.trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('with no device, warns and emits no operation (import)', async () => {
    const { w, spy } = await mountLoaded(() => ({ ...DATA, devices: [] }))
    const file = new File(['[[bindings]]\ncode = 1\ncmd = "Mute"\n'], 'p.toml')
    const input = w.find('[data-import]').element as HTMLInputElement
    Object.defineProperty(input, 'files', { value: [file] })
    await w.find('[data-import]').trigger('change')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('with no device, warns and emits no operation (export)', async () => {
    const { w, spy } = await mountLoaded(() => ({ ...DATA, devices: [] }))
    const button = w.findAll('button').find((b) => b.text() === CATALOG.btn_export)!
    await button.trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('refresh sends `rescan` then reloads', async () => {
    const { w, spy } = await mountLoaded()
    await w.find('[data-refresh]').trigger('click')
    await flushPromises()
    const ops = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    expect(ops).toEqual(['rescan'])
  })

  it('revalidates the selected preset when it disappears from the new list', async () => {
    let presets = ['mce', 'keyboard']
    const { w } = await mountLoaded(() => ({ ...DATA, presets }))
    const presetSelect = w.findAllComponents(Select)[1]!
    expect(presetSelect.props('modelValue')).toBe('mce')
    presets = ['keyboard'] // "mce" disappears from the served list
    await w.find('[data-refresh]').trigger('click')
    await flushPromises()
    expect(presetSelect.props('modelValue')).toBe('keyboard')
  })

  it('imports a `.toml` file by handing it to the server', async () => {
    const { w, spy } = await mountLoaded()
    const file = new File(['[[bindings]]\ncode = 1\ncmd = "Mute"\n'], 'p.toml')
    const input = w.find('[data-import]').element as HTMLInputElement
    Object.defineProperty(input, 'files', { value: [file] })
    await w.find('[data-import]').trigger('change')
    await vi.waitFor(() =>
      expect(
        spy.mock.calls.some(
          (c) =>
            (c[1] as RequestInit)?.method === 'PUT' &&
            JSON.parse(String((c[1] as RequestInit).body)).op === 'import_preset',
        ),
      ).toBe(true),
    )
  })

  it('validation: a code already carried by another action puts both rows in error', async () => {
    const { w } = await conflictBetweenTwoRows()
    // The red ring and border come from the kit's `Input`, which already
    // carries `aria-invalid:border-destructive`: the attribute is the
    // whole signal.
    expect(actionRow(w, 'Muet').find('input').attributes('aria-invalid')).toBe('true')
    expect(actionRow(w, 'Veille').find('input').attributes('aria-invalid')).toBe('true')
    // Each row names the **other** action, by its translated label and
    // never by its i18n key.
    const muteText = actionRow(w, 'Muet').find('[data-conflict]').text()
    const standbyText = actionRow(w, 'Veille').find('[data-conflict]').text()
    expect(muteText).toContain('Veille')
    expect(muteText).not.toContain('act_power')
    expect(standbyText).toContain('Muet')
    expect(standbyText).not.toContain('act_mute')
  })

  it('validation: the conflict message names the faulty code', async () => {
    const { w } = await conflictBetweenTwoRows()
    expect(actionRow(w, 'Veille').find('[data-conflict]').text()).toBe('le code 9 est déjà affecté à Muet')
  })

  it('validation: a duplicate internal to the field is reported without naming an action', async () => {
    const { w } = await mountLoaded()
    await actionRow(w, 'Muet').find('input').setValue('9, 9')
    const message = actionRow(w, 'Muet').find('[data-conflict]')
    expect(message.exists()).toBe(true)
    expect(message.text()).toBe('le code 9 est saisi deux fois')
    // No other action is at fault: the message must name no one.
    expect(message.text()).not.toContain('Muet')
  })

  it('validation: as long as a conflict exists, "Save" is disabled and emits nothing', async () => {
    const { w, spy } = await conflictBetweenTwoRows()
    const save = w.find('[data-save]')
    expect(save.attributes('disabled')).toBeDefined()
    // A greyed-out button with no sentence explains nothing.
    expect(w.find('[data-save-blocked]').text()).toBe(CATALOG.save_conflicts)
    await save.trigger('click')
    await flushPromises()
    expect(ops(spy)).toEqual([])
  })

  it('validation: clearing the faulty code removes the error and re-enables "Save"', async () => {
    const { w } = await conflictBetweenTwoRows()
    // The conflict exists **before** clearing: without this assertion, the
    // test would pass just as well if `[data-conflict]` were never rendered.
    expect(w.findAll('[data-conflict]')).toHaveLength(2)
    await actionRow(w, 'Veille').find('[data-clear]').trigger('click')
    expect(w.findAll('[data-conflict]')).toHaveLength(0)
    expect(w.find('[data-save]').attributes('disabled')).toBeUndefined()
    expect(w.find('[data-save-blocked]').exists()).toBe(false)
  })

  it('validation: a code arriving via learning triggers live validation like a keystroke', async () => {
    // The seam between the popup and live validation: `applyCode` writes
    // into the same `codes`, so the `computed` must recompute. Nothing was
    // holding this -- all the conflict tests went through `setValue`, all
    // the learning tests through a free code. "Muet" already carries 9:
    // capturing it on "Veille" must put both rows in error.
    const { w, row } = await learnAndCapture('Veille', 9)
    expect(w.findAll('[data-conflict]')).toHaveLength(2)
    // The learned row names the other action, and the other names it back.
    // `row()` comes from the scenario: it is also what keeps its row
    // lookup on the first cell -- a `text().includes('Veille')` would
    // return the "Muet" row here, whose conflict message names "Veille".
    expect(row().find('[data-conflict]').text()).toContain('Muet')
    expect(actionRow(w, 'Muet').find('[data-conflict]').text()).toContain('Veille')
    expect(w.find('[data-save]').attributes('disabled')).toBeDefined()
  })

  it('validation: a healthy table loaded from the server shows no conflict', async () => {
    // Guard against a false positive on mount: the 22 empty fields are not
    // 22 times the same code.
    const { w } = await mountLoaded()
    expect(w.findAll('[data-conflict]')).toHaveLength(0)
    expect(w.find('[data-save]').attributes('disabled')).toBeUndefined()
  })
})
