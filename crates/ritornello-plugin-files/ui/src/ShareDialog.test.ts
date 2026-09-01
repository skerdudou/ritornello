// Same direct mounting as `DeviceDialog`, and for the same reason:
// `SourcesPane`, which will open this dialog, does not exist yet. The
// content goes into a portal to `document.body` — `wrapper.find()` never
// sees it, and without `attachTo` it is not even rendered.
import { flushPromises, mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { afterEach, describe, expect, it, vi } from 'vitest'
import ShareDialog from './ShareDialog.vue'
import { normalizeData, type Send } from './data'
import {
  CATALOG,
  EXPLORE_CLOSED,
  clickPopover,
  inPopover,
  state,
  cleanupPopovers,
  typeInPopover,
} from './harness'
import type { ServerState } from './harness'

const t = createT(CATALOG)

afterEach(cleanupPopovers)

/** The initial state is declared in **server** shape (`snake_case`). */
async function mountDialog(partial: ServerState = {}, message = '') {
  const data = normalizeData(state({ can_browse_smb: true, ...partial }))
  const send = vi.fn<Send>().mockResolvedValue(data)
  const w = mount(ShareDialog, {
    props: { data, t, send, frozen: false, open: true, message },
    attachTo: document.body,
  })
  // The portal is only populated on the cycle following the mount.
  await flushPromises()
  return { w, send }
}

/** Wizard connected to a host, in either of its two stages. */
function connected(overrides: Record<string, unknown>) {
  return {
    explore: { ...EXPLORE_CLOSED, open: true, kind: 'smb' as const, host: 'nas', ...overrides },
  }
}

describe('ShareDialog', () => {
  it('connecting sends the host and the credentials only once', async () => {
    const { send } = await mountDialog()
    await typeInPopover('[data-host]', '192.168.1.20')
    await typeInPopover('[data-user]', 'steven')
    await typeInPopover('[data-password]', 'secret')
    await clickPopover('[data-connect]')
    expect(send).toHaveBeenCalledWith({
      op: 'smb_connect',
      host: '192.168.1.20',
      user: 'steven',
      password: 'secret',
      domain: '',
    })
    expect(send).toHaveBeenCalledTimes(1)
  })

  it('choosing a share requests its root', async () => {
    const { send } = await mountDialog(connected({ shares: ['musique'] }))
    await clickPopover('[data-share]')
    expect(send).toHaveBeenCalledWith({ op: 'smb_browse', share: 'musique', path: '' })
  })

  it('descending keeps the share and composes a relative path', async () => {
    // Relative to the share, not absolute: that is what `smbclient -D`
    // expects, and a leading slash would make it start over from the
    // share's root.
    const { send } = await mountDialog(
      connected({ share: 'musique', path: 'Ma Musique', dirs: ['Jazz'] }),
    )
    await clickPopover('[data-picker-folder]')
    expect(send).toHaveBeenCalledWith({
      op: 'smb_browse',
      share: 'musique',
      path: 'Ma Musique/Jazz',
    })
  })

  it('confirming declares the source without asking for the password again', async () => {
    // It was just used to connect: retyping it would be absurd, and the
    // page never received it back anyway.
    const { send } = await mountDialog(
      connected({ share: 'musique', path: 'Ma Musique', shares: ['musique'], dirs: [] }),
    )
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({
      op: 'add_source',
      kind: 'smb',
      path: null,
      host: 'nas',
      share: 'musique',
      subpath: 'Ma Musique',
      user: '',
      domain: '',
      password: '',
      writable: false,
    })
  })

  it('without smbclient the dialog is straight away in manual input', async () => {
    // No more greyed-out button to understand: there is nothing to browse,
    // hence nothing to toggle. The reason stays named, it explains why the
    // fields replace the wizard.
    const { send } = await mountDialog({ can_browse_smb: false })
    expect((inPopover('[data-smb-unavailable]')?.textContent ?? '').length).toBeGreaterThan(0)
    expect(inPopover('[data-manual-share]')).not.toBeNull()
    expect(inPopover('[data-connect]')).toBeNull()
    expect(inPopover('[data-manual]')).toBeNull()
    expect(send).not.toHaveBeenCalled()
  })

  it('with smbclient the manual toggle stays offered, and the wizard is the default', async () => {
    await mountDialog({ can_browse_smb: true })
    expect(inPopover('[data-manual-share]')).toBeNull()
    expect(inPopover('[data-manual]')).not.toBeNull()
    await clickPopover('[data-manual]')
    expect(inPopover('[data-manual-share]')).not.toBeNull()
  })

  it('the domain field says it is optional', async () => {
    // Reported from use: "domain" alone does not say what it is for, and
    // reads like a field to fill in. It is only used for a Windows domain
    // account.
    await mountDialog({ can_browse_smb: true })
    expect(inPopover('[data-domain]')?.getAttribute('placeholder')).toContain('optional')
  })

  it('the manual input declares the source directly', async () => {
    const { send } = await mountDialog({ can_browse_smb: false })
    await typeInPopover('[data-host]', 'nas')
    await typeInPopover('[data-manual-share]', 'musique')
    await typeInPopover('[data-manual-subpath]', 'Albums')
    await typeInPopover('[data-user]', 'steven')
    await typeInPopover('[data-password]', 'secret')
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({
      op: 'add_source',
      kind: 'smb',
      path: null,
      host: 'nas',
      share: 'musique',
      subpath: 'Albums',
      user: 'steven',
      domain: '',
      password: 'secret',
      writable: false,
    })
  })

  it('an empty manual subpath is sent as null, never as an empty string', async () => {
    // On the plugin side this is an `Option<String>`: `Some("")` is not "no
    // subpath" but an empty subpath, which validation refuses — the share
    // would then be undeclarable with no field looking at fault.
    const { send } = await mountDialog({ can_browse_smb: false })
    await typeInPopover('[data-host]', 'nas')
    await typeInPopover('[data-manual-share]', 'musique')
    await clickPopover('[data-choose]')
    expect(send.mock.calls[0]?.[0]).toMatchObject({ subpath: null })
  })

  it('a refusal displays instead of an empty share list', async () => {
    // A silent dialog after clicking "Connect" reads like a connection that
    // never happened.
    await mountDialog(connected({ error: 'host unreachable' }))
    expect(inPopover('[data-share-error]')?.textContent ?? '').toContain('host unreachable')
    expect(inPopover('[data-share]')).toBeNull()
  })

  it('the share stays visible in the path when descending into it', async () => {
    // Reported defect: `explore.path` is relative to the share, so the
    // chosen share appeared nowhere — it seemed "eaten" as soon as it was
    // entered, with nothing saying which one was being browsed.
    await mountDialog({
      explore: {
        ...EXPLORE_CLOSED,
        open: true,
        kind: 'smb',
        host: '192.168.1.15',
        share: 'music',
        path: 'Yann Tiersen',
        shares: ['music'],
      },
    })
    expect(inPopover('[data-picker-path]')?.getAttribute('title')).toBe(
      '//192.168.1.15/music/Yann Tiersen',
    )
  })

  it('at the top of a share, goUp returns to the share list', async () => {
    // Reported defect: there, goUp did nothing at all, and closing the
    // dialog was needed to try another share.
    const { send } = await mountDialog(connected({ share: 'music', path: '' }))
    await clickPopover('[data-picker-go-up]')
    expect(send).toHaveBeenCalledWith({ op: 'smb_shares' })
  })

  it('an explicit button also returns to the share list', async () => {
    const { send } = await mountDialog(connected({ share: 'music', path: 'Yann Tiersen' }))
    await clickPopover('[data-to-shares]')
    expect(send).toHaveBeenCalledWith({ op: 'smb_shares' })
  })

  it('returning to the shares triggers no network call', async () => {
    // `smb_shares` and not `smb_connect`: the shares are already known, and
    // redoing the call would make a simple return wait — or even fail.
    const { send } = await mountDialog(connected({ share: 'music', path: 'Yann Tiersen' }))
    await clickPopover('[data-to-shares]')
    expect(send.mock.calls.some((c) => (c[0] as { op: string }).op === 'smb_connect')).toBe(false)
  })

  it('reopening the dialog keeps nothing from the previous input', async () => {
    // The `Dialog` stays mounted when closed: without this reset, the host
    // and the password from the previous time would reappear — a secret
    // that has no business staying in memory once the dialog is closed
    // again.
    const { w } = await mountDialog()
    await typeInPopover('[data-host]', '192.168.1.15')
    await typeInPopover('[data-password]', 'secret-du-nas')
    await w.setProps({ open: false })
    await w.setProps({ open: true })
    expect((inPopover('[data-host]') as HTMLInputElement).value).toBe('')
    expect((inPopover('[data-password]') as HTMLInputElement).value).toBe('')
  })

  it('displays the plugin refusal in the dialog, not only on the page', async () => {
    // Same defect as for the local wizard: the page's banner lives behind
    // the dialog's grey veil, hence unreadable when it matters. This path
    // carries `add_source` refusals — a duplicate, for instance — which
    // `explore.error` does not carry.
    const refusal = 'This folder is already declared as a source.'
    await mountDialog(connected({}), refusal)
    expect(inPopover('[data-dlg-message]')?.textContent).toContain(refusal)
  })
})
