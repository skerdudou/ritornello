import { flushPromises, mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { h } from 'vue'
import {
  Button, cn, Dialog, DialogContent, DialogTitle, Slider, Tabs, TabsContent, TabsList,
  TabsTrigger, UI_CONTRACT,
} from './index'
// Not part of the kit's public surface, and no reason to widen it just for
// a test that lives inside the kit.
import { buttonVariants } from './components/ui/button'

// jsdom does not provide ResizeObserver; reka-ui uses it (`useSize`) to
// measure the slider track on mount. A minimal stub is enough: the test checks
// no pixel-accurate position.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
vi.stubGlobal('ResizeObserver', ResizeObserverStub)

describe('public surface of the kit', () => {
  it('exposes the contract version', () => {
    expect(UI_CONTRACT).toBe(1)
  })

  it('cn merges classes, the last one winning', () => {
    expect(cn('p-2', 'p-4')).toBe('p-4')
  })

  it('mounts a Button with its content', () => {
    const w = mount(Button, { slots: { default: 'Save' } })
    expect(w.text()).toBe('Save')
    expect(w.element.tagName).toBe('BUTTON')
  })

  it('tabs only mount the active panel, and clicking changes it', async () => {
    // Unmounting inactive panels is not a display detail: a page that puts
    // panes in tabs stops keeping alive what is not being looked at. Better
    // checked here than discovered through a poll that stops on its own.
    const w = mount(
      {
        components: { Tabs, TabsList, TabsTrigger, TabsContent },
        template: `
          <Tabs default-value="a">
            <TabsList>
              <TabsTrigger value="a">One</TabsTrigger>
              <TabsTrigger value="b">Two</TabsTrigger>
            </TabsList>
            <TabsContent value="a">panel A</TabsContent>
            <TabsContent value="b">panel B</TabsContent>
          </Tabs>`,
      },
      { attachTo: document.body },
    )
    await flushPromises()
    expect(w.text()).toContain('panel A')
    expect(w.text()).not.toContain('panel B')

    // Focus **then** click: reka-ui activates the tab on focus ("automatic"
    // mode), which a real click always produces but which `trigger('click')`
    // alone does not under jsdom.
    const second = w.findAll('[data-slot="tabs-trigger"]')[1]!
    ;(second.element as HTMLElement).focus()
    await second.trigger('click')
    await flushPromises()
    expect(w.text()).toContain('panel B')
    expect(w.text()).not.toContain('panel A')
    w.unmount()
  })

  it('the slider renders an accessible thumb and commits a keyboard step', async () => {
    // A single component for progress and volume: what is checked here — the
    // thumb is a `role=slider`, a keyboard step emits the value **and** commits
    // it — is what both usages assume.
    const w = mount(Slider, {
      props: { modelValue: [60], min: 0, max: 100, step: 1, 'aria-label': 'Volume' },
      attachTo: document.body,
    })
    await flushPromises()
    const thumb = w.get('[role="slider"]')
    expect(thumb.attributes('aria-valuenow')).toBe('60')
    expect(thumb.attributes('aria-valuemin')).toBe('0')
    expect(thumb.attributes('aria-valuemax')).toBe('100')
    // Without the attrs sorting in Slider.vue, `aria-label` leaks to the
    // enclosing `<span>` of `SliderRoot` and the thumb is left without an
    // accessible name: it is the thumb, not the root, that this test checks.
    expect(thumb.attributes('aria-label')).toBe('Volume')
    ;(thumb.element as HTMLElement).focus()
    await thumb.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('update:modelValue')?.[0]).toEqual([[61]])
    expect(w.emitted('valueCommit')?.[0]).toEqual([[61]])
    // "sent once": a single keyboard step must not produce several commits.
    expect(w.emitted('valueCommit')).toHaveLength(1)
    w.unmount()
  })

  /**
   * Guardrails on the hand cursor.
   *
   * Tailwind's Preflight no longer puts `cursor: pointer` on a `<button>`, so
   * every clickable of this kit carries the class explicitly. Nothing enforced
   * that, and the dialog's close cross -- built from raw classes instead of
   * going through `buttonVariants` -- had silently missed it: on a popin, the
   * one control that closes it gave no hover feedback at all.
   *
   * Asserted on the class and not on a computed style: jsdom computes none.
   * That the browser really applies it was measured through Playwright.
   */
  describe('the hand cursor on what is clickable', () => {
    it('every button variant carries it', () => {
      expect(buttonVariants()).toContain('cursor-pointer')
      expect(buttonVariants({ variant: 'ghost', size: 'icon-xs' })).toContain('cursor-pointer')
    })

    it('the close cross of a dialog carries it', async () => {
      const w = mount(Dialog, {
        props: { open: true },
        slots: { default: () => h(DialogContent, null, { default: () => h(DialogTitle, null, { default: () => 'Title' }) }) },
        attachTo: document.body,
      })
      await flushPromises()
      // The dialog renders in a portal, hence `document.body` and not `w`.
      const close = document.body.querySelector('[data-slot="dialog-close"]')
      expect(close).not.toBeNull()
      expect(close!.className).toContain('cursor-pointer')
      w.unmount()
      document.body.innerHTML = ''
    })
  })
})
