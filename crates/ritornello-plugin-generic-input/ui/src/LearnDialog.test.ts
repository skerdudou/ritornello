// The learning popup is teleported to a portal on `document.body` (like
// every kit `Dialog`): `wrapper.find()` never sees it, it must be mounted
// with `attachTo: document.body` and looked up in the document. Portals are
// not cleaned up by the wrapper's unmount, hence the `beforeEach`.
import { createT, Dialog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it } from 'vitest'
import LearnDialog from './LearnDialog.vue'

const CATALOG: Record<string, string> = {
  dlg_learn_title: 'Key learning',
  dlg_learn_desc: 'Press a key on the device “{device}”…',
  learn_append_label: 'Add to the existing codes instead of replacing them',
  learn_countdown: '{s} s left',
  btn_cancel: 'Cancel',
}
// The kit's real translator, not a stub that would render the raw value:
// it is the one that interpolates the `{name}` tokens, and the popup must
// pass it its parameters rather than substituting them itself.
const t = createT(CATALOG)

interface Props {
  open: boolean
  t: typeof t
  action: string
  device: string
  add: boolean
  seconds: number
}

beforeEach(() => {
  document.body.innerHTML = ''
})

function mountDialog(props: Partial<Props> = {}) {
  return mount(LearnDialog, {
    props: {
      open: true,
      t,
      action: 'Muet',
      device: 'mce',
      add: false,
      seconds: 30,
      ...props,
    },
    attachTo: document.body,
  })
}

function inPopup(selector: string) {
  return document.body.querySelector(selector)
}

describe('LearnDialog', () => {
  it('closed, it puts nothing in the document', async () => {
    mountDialog({ open: false })
    await flushPromises()
    expect(inPopup('[data-dlg-learn]')).toBeNull()
  })

  it('open, the title carries the action and the description names the device', async () => {
    mountDialog({ action: 'Muet', device: 'mce' })
    await flushPromises()
    const popup = inPopup('[data-dlg-learn]')
    expect(popup).not.toBeNull()
    expect(popup!.textContent).toContain('Muet')
    expect(popup!.textContent).toContain('mce')
    // The token has not leaked as-is: the description did replace it.
    expect(popup!.textContent).not.toContain('{device}')
  })

  it('with no action to name, the title carries no trailing dash', async () => {
    // The page clears `action` as soon as the closing gesture happens, while
    // reka-ui keeps the content mounted for the duration of the exit fade:
    // during those 200 ms, the title must not read "Key learning —".
    mountDialog({ action: '' })
    await flushPromises()
    expect(inPopup('[data-slot="dialog-title"]')!.textContent).toBe('Key learning')
  })

  it('the Cancel button emits exactly one "cancel"', async () => {
    const w = mountDialog()
    await flushPromises()
    ;(inPopup('[data-learn-cancel]') as HTMLButtonElement).click()
    await flushPromises()
    expect(w.emitted('cancel')).toHaveLength(1)
  })

  it('closing via `update:open` emits exactly one "cancel"', async () => {
    // Escape, the click on the overlay and the kit's close icon all go
    // through this single path; `[data-learn-cancel]`, which emits `cancel`
    // directly, never exercises it.
    const w = mountDialog()
    await flushPromises()
    w.findComponent(Dialog).vm.$emit('update:open', false)
    await flushPromises()
    expect(w.emitted('cancel')).toHaveLength(1)
  })

  it('the close icon placed by `DialogContent` also cancels', async () => {
    // The kit renders a `DialogClose` as soon as `showCloseButton` is not
    // negated — and it defaults to `true`. Third cancellation trigger, the
    // one no line of this popup's template makes visible.
    const w = mountDialog()
    await flushPromises()
    const closeIcon = inPopup('[data-slot="dialog-close"]') as HTMLButtonElement | null
    expect(closeIcon).not.toBeNull()
    closeIcon!.click()
    await flushPromises()
    expect(w.emitted('cancel')).toHaveLength(1)
  })

  it('checking the box emits update:add with true, with no state of its own', async () => {
    const w = mountDialog({ add: false })
    await flushPromises()
    const checkbox = inPopup('[data-learn-append]') as HTMLInputElement
    expect(checkbox.checked).toBe(false)
    checkbox.checked = true
    checkbox.dispatchEvent(new Event('change'))
    await flushPromises()
    expect(w.emitted('update:add')).toEqual([[true]])
  })

  it('add: true shows the box checked', async () => {
    mountDialog({ add: true })
    await flushPromises()
    expect((inPopup('[data-learn-append]') as HTMLInputElement).checked).toBe(true)
  })

  it('shows the time left to press', async () => {
    // Without a countdown, the popup would close itself after 30 s with
    // nothing having warned of the deadline: the user would think the
    // device is silent when they simply took too long to find the key.
    mountDialog({ seconds: 27 })
    await flushPromises()
    expect(inPopup('[data-learn-countdown]')?.textContent).toContain('27')
  })

  it('shows no countdown once the deadline is reached', async () => {
    // At zero the page has already stopped learning: displaying "0 s left"
    // during the close would be a countdown that lies.
    mountDialog({ seconds: 0 })
    await flushPromises()
    expect(inPopup('[data-learn-countdown]')).toBeNull()
  })
})
