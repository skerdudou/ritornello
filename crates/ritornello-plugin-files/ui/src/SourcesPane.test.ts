import { flushPromises } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { inPopover, mountAdmin, cleanupPopovers } from './harness'

const USB = {
  name: 'usb',
  kind: 'local',
  path: '/media/usb',
  host: '',
  share: '',
  user: '',
  domain: '',
  writable: false,
  mounted: true,
}

const NAS = {
  name: 'musique',
  kind: 'smb',
  host: '192.168.1.15',
  share: 'music',
  subpath: 'Yann Tiersen',
  user: 'ritornello',
  domain: '',
  writable: false,
  mounted: true,
}

describe('sources pane', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(cleanupPopovers)

  it('with no source, invites adding one rather than leaving an empty space', async () => {
    const { w } = await mountAdmin()
    expect(w.find('[data-no-sources]').exists()).toBe(true)
    expect(w.findAll('[data-source-row]')).toHaveLength(0)
  })

  it('displays a source target and the observed state of its mount', async () => {
    const { w } = await mountAdmin({ roots: [NAS] })
    expect(w.find('[data-source-target]').text()).toBe('//192.168.1.15/music/Yann Tiersen')
    expect(w.find('[data-source-mounted]').text()).toBe('mounted')
    expect(w.find('[data-source-kind]').text()).toBe('network share')
  })

  it('adds a whole source to the queue in one click', async () => {
    // The explicit requirement: from the declared sources, "add all" must
    // be immediately at hand. The empty path designates the whole source.
    const { w, s } = await mountAdmin({ roots: [USB] })
    await w.find('[data-add-all]').trigger('click')
    await flushPromises()
    expect(s.putsOf('add_dir')[0]).toEqual({ op: 'add_dir', root: 'usb', path: '' })
  })

  it('removes a source by naming it', async () => {
    const { w, s } = await mountAdmin({ roots: [USB] })
    await w.find('[data-remove-source]').trigger('click')
    await flushPromises()
    expect(s.putsOf('remove_source')[0]).toEqual({ op: 'remove_source', name: 'usb' })
  })

  it('toggles writability without going through a redeclaration', async () => {
    // A separate operation, and that is the point: without it, changing
    // one's mind would require removing then redeclaring the source, hence
    // retyping the password that the page does not know.
    const { w, s } = await mountAdmin({ roots: [NAS] })
    const checkbox = w.find('[data-writable]')
    await checkbox.setValue(true)
    await flushPromises()
    expect(s.putsOf('set_writable')[0]).toEqual({
      op: 'set_writable',
      name: 'musique',
      writable: true,
    })
  })

  it('only offers writability on a share', async () => {
    // A device folder is writable or not depending on the filesystem; the
    // switch only drives the cifs mount options, it would mean nothing
    // here.
    const { w } = await mountAdmin({ roots: [USB] })
    expect(w.find('[data-writable]').exists()).toBe(false)
    expect(w.find('[data-source-mounted]').exists()).toBe(false)
  })

  it('shows a mount failure and allows retrying it', async () => {
    // Mounting follows the declaration: without this report, a source
    // would stay "not mounted" with nothing saying why.
    const { w, s } = await mountAdmin({
      roots: [{ ...NAS, mounted: false }],
      mount_error: 'Interactive authentication required.',
    })
    expect(w.find('[data-mount-error]').text()).toContain('Interactive authentication required.')
    await w.find('[data-retry-mount]').trigger('click')
    await flushPromises()
    expect(s.putsOf('mount')).toHaveLength(1)
  })

  it('shows no mount banner when everything is fine', async () => {
    const { w } = await mountAdmin({ roots: [NAS] })
    expect(w.find('[data-mount-error]').exists()).toBe(false)
    expect(w.find('[data-retry-mount]').exists()).toBe(false)
    expect(w.find('[data-unresponsive]').exists()).toBe(false)
  })

  it('tells a silent mount apart from a mount failure', async () => {
    // Two different failures, two different messages. A share that no
    // longer responds **is** mounted: confusing it with a mount failure
    // would send the user retrying a mount that succeeded. This is also the
    // block that gives the cause of the "unclear" states of the queue.
    const { w } = await mountAdmin({
      roots: [NAS],
      mount_error: null,
      unresponsive: ['/mnt/ritornello/musique'],
    })
    const block = w.find('[data-unresponsive]')
    expect(block.exists()).toBe(true)
    expect(block.text()).toContain('/mnt/ritornello/musique')
    // The mount retry has no business being there: the root is mounted.
    expect(w.find('[data-mount-error]').exists()).toBe(false)
  })

  it('offers the retry on an unmounted source, even without a remembered error', async () => {
    // Reported defect: on the application's restart, `mount_error` resets
    // empty — it describes the *last attempt*, and there was none. So
    // "not mounted" was found again with nothing left to fix it. The retry
    // follows the state **observed** in /proc/mounts, which survives a
    // restart.
    const { w, s } = await mountAdmin({ roots: [{ ...NAS, mounted: false }], mount_error: null })
    expect(w.find('[data-mount-error]').exists()).toBe(false)
    await w.find('[data-retry-mount]').trigger('click')
    await flushPromises()
    expect(s.putsOf('mount')).toHaveLength(1)
  })

  it('offers no retry on a device folder', async () => {
    // There is nothing to mount: `mount::state` always renders "mounted"
    // for a local root, and offering a retry would suggest a problem.
    const { w } = await mountAdmin({ roots: [{ ...USB, mounted: false }] })
    expect(w.find('[data-retry-mount]').exists()).toBe(false)
  })

  it('opens the device wizard while notifying the plugin', async () => {
    // Opening goes through the plugin, not only through a local boolean: it
    // is the plugin that carries the wizard's state, and a dialog
    // displaying without notifying it would inherit the previous one's
    // state.
    const { w, s } = await mountAdmin({ volumes: [{ path: '/media/usb', fstype: 'vfat' }] })
    await w.find('[data-add-device]').trigger('click')
    await flushPromises()
    expect(s.putsOf('explore_open')[0]).toEqual({ op: 'explore_open', kind: 'local' })
    // The dialog's content lives in a portal: `w.find` would never see it
    // there, whatever the component's state.
    expect(inPopover('[data-device-dialog]')).not.toBeNull()
  })

  it('opens the network wizard while notifying the plugin', async () => {
    const { w, s } = await mountAdmin({ can_browse_smb: true })
    await w.find('[data-add-share]').trigger('click')
    await flushPromises()
    expect(s.putsOf('explore_open')[0]).toEqual({ op: 'explore_open', kind: 'smb' })
    expect(inPopover('[data-share-dialog]')).not.toBeNull()
  })

  it('both dialogs stay closed as long as they are not opened', async () => {
    const { w } = await mountAdmin({ roots: [USB] })
    expect(w.find('[data-sources-pane]').exists()).toBe(true)
    expect(inPopover('[data-device-dialog]')).toBeNull()
    expect(inPopover('[data-share-dialog]')).toBeNull()
  })
})
