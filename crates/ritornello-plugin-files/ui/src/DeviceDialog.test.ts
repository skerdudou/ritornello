// The wizard mounts **directly**, and not through the page: `SourcesPane`,
// which will open it, does not exist yet. Direct mounting still requires
// `attachTo: document.body` and `inPopover` — a `Dialog`'s content goes into
// a portal to `document.body`, so `wrapper.find()` never finds it, and
// without attaching it is not even rendered.
import { flushPromises, mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DeviceDialog from './DeviceDialog.vue'
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

// Portals are not cleaned up by unmounting the wrapper: without this, a
// previous test's dialog would stay in the document and the next test would
// query the wrong panel.
afterEach(cleanupPopovers)

/**
 * The initial state is declared in **server** shape (`snake_case`), as the
 * plugin serializes it, then goes through `normalizeData`: that is the only
 * path the real page takes, and the only one that exercises normalization
 * together with the template.
 */
async function mountDialog(partial: ServerState = {}, message = '') {
  const data = normalizeData(
    state({ volumes: [{ path: '/media/usb', fstype: 'vfat' }], ...partial }),
  )
  const send = vi.fn<Send>().mockResolvedValue(data)
  const w = mount(DeviceDialog, {
    props: { data, t, send, frozen: false, open: true, message },
    attachTo: document.body,
  })
  // The portal is only populated on the cycle following the mount:
  // querying the document right away finds nothing, and would make it look
  // like a template defect when the dialog simply arrives one frame later.
  await flushPromises()
  return { w, send }
}

/** Wizard open on a volume, hence already in the tree. */
function inTree(path: string, dirs: string[]) {
  return { explore: { ...EXPLORE_CLOSED, open: true, kind: 'local' as const, path, dirs } }
}

describe('DeviceDialog', () => {
  it('offers the mounted volumes', async () => {
    // It opens on the volumes and never on `/`: nobody knows the absolute
    // path of a USB key, and that is nonetheless what the old form asked
    // for typing.
    const { w } = await mountDialog()
    // The dialog lives in a portal: `w.find` NEVER sees it.
    expect(w.find('[data-volume]').exists()).toBe(false)
    expect(inPopover('[data-volume]')?.textContent ?? '').toContain('/media/usb')
  })

  it('choosing a volume requests its content from the plugin', async () => {
    const { send } = await mountDialog()
    await clickPopover('[data-volume]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb' })
  })

  it('descending composes the absolute path', async () => {
    // The tree only emits a name: this is where the path is composed, and a
    // local path is composed with forward slashes.
    const { send } = await mountDialog(inTree('/media/usb', ['Albums']))
    await clickPopover('[data-picker-folder]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('goUp goes down one level as long as we stay in the volume', async () => {
    const { send } = await mountDialog(inTree('/media/usb/Albums/Jazz', []))
    await clickPopover('[data-picker-go-up]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('at the top of a volume, goUp returns to the volume list', async () => {
    // Reported defect: once a volume was chosen, another one could no longer
    // be tried. Going up led into `/media` then `/` — leaving the volume
    // without ever finding the list again, forcing the dialog to be closed.
    const { send } = await mountDialog(inTree('/media/usb', []))
    await clickPopover('[data-picker-go-up]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('an explicit button also returns to the volume list', async () => {
    // The way back must not be guessable: going up to the top hoping to
    // land back on the list is not a manoeuvre one invents.
    const { send } = await mountDialog(inTree('/media/usb/Albums', []))
    await clickPopover('[data-to-volumes]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('a path outside any known volume returns to the list', async () => {
    // Rather than going up blindly: if the path cannot be placed within a
    // declared volume, the list is the only safe landmark.
    const { send } = await mountDialog(inTree('/elsewhere', []))
    await clickPopover('[data-picker-go-up]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('a path typed by hand opens that folder instead of declaring it', async () => {
    // Navigating rather than declaring, on purpose: this keeps the
    // verification that is the whole point of the dialog — the folder's
    // content and its audio file count — before confirming anything.
    const { send } = await mountDialog()
    await typeInPopover('[data-manual-path]', '  /srv/musique  ')
    await clickPopover('[data-manual-go]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/srv/musique' })
  })

  it('the manual input stays offered once inside the tree', async () => {
    // This is even where it is most useful: the wrong branch was taken and
    // a jump elsewhere is wanted without going up click by click.
    await mountDialog(inTree('/media/usb/Albums', []))
    expect(inPopover('[data-manual-path]')).not.toBeNull()
  })

  it('an empty input triggers nothing', async () => {
    // Otherwise the button would send an `explore_local` on the empty
    // string, which the plugin would refuse — a refusal caused by the UI
    // itself.
    const { send } = await mountDialog()
    await typeInPopover('[data-manual-path]', '   ')
    expect(inPopover('[data-manual-go]')?.hasAttribute('disabled')).toBe(true)
    expect(send).not.toHaveBeenCalled()
  })

  it('reopening the dialog keeps nothing from the previous input', async () => {
    // The `Dialog` stays mounted when closed: without this reset, the path
    // typed the previous time would reappear, as if it had never been
    // closed.
    const { w } = await mountDialog()
    await typeInPopover('[data-manual-path]', '/srv/musique')
    await w.setProps({ open: false })
    await w.setProps({ open: true })
    expect((inPopover('[data-manual-path]') as HTMLInputElement).value).toBe('')
  })

  it('displays the plugin refusal in the dialog, not only on the page', async () => {
    // Reported defect: the message landed on the main page, behind the
    // dialog's grey veil — hence unreadable at the precise moment it
    // matters, when a forbidden folder was just chosen.
    const refusal = 'This path is not browsable: /root/private'
    const { w } = await mountDialog(inTree('/media/usb', []), refusal)
    expect(inPopover('[data-dlg-message]')?.textContent).toContain(refusal)
    void w
  })

  it('confirming declares the source with the current path', async () => {
    const { send } = await mountDialog(inTree('/media/usb/Albums', []))
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({
      op: 'add_source',
      kind: 'local',
      path: '/media/usb/Albums',
      host: '',
      share: '',
      subpath: null,
      user: '',
      domain: '',
      password: '',
      writable: false,
    })
  })

  it('confirming closes the wizard on the plugin side', async () => {
    // Without `explore_close`, the wizard state would stay open on the
    // plugin side: the dialog would reopen on its own at the next page load.
    const { w, send } = await mountDialog(inTree('/media/usb/Albums', []))
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_close' })
    expect(w.emitted('close')).toHaveLength(1)
  })

  it('confirming is out of reach as long as no volume is chosen', async () => {
    // Declaring a source without a path would be a refusal from the plugin
    // in front of a button that looked ready.
    const { send } = await mountDialog()
    expect(inPopover('[data-choose]')?.getAttribute('disabled')).not.toBeNull()
    expect(send).not.toHaveBeenCalled()
  })

  it('with no volume the dialog says so instead of offering an empty list', async () => {
    const { w } = await mountDialog({ volumes: [] })
    expect(inPopover('[data-no-volumes]')).not.toBeNull()
    expect(inPopover('[data-volume]')).toBeNull()
    expect(w.find('[data-no-volumes]').exists()).toBe(false)
  })
})
