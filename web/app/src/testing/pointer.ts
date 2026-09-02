import { nextTick } from 'vue'

/**
 * Dispatches a real `PointerEvent` carrying its coordinates, and waits for the
 * render that follows.
 *
 * `wrapper.trigger('pointermove', { clientX: 100 })` cannot be used for this.
 * `@vue/test-utils` builds the event from the options and then copies every
 * option onto the instance, guarding itself by looking for a setter — but it
 * only inspects the *immediate* prototype:
 *
 *     const descriptor = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(event), key)
 *
 * `clientX` lives on `MouseEvent.prototype`, one rung above
 * `PointerEvent.prototype`, so the guard finds nothing, assigns, and jsdom 30
 * throws `Cannot set property clientX of #<MouseEvent> which has only a
 * getter`. jsdom 25 was silent about it for the wrong reason: it did not
 * implement `PointerEvent` at all, so test-utils fell back to `window.Event`
 * and `clientX` was an invented property on an event of the wrong type.
 *
 * Passing the coordinates to the constructor instead is both immune to that
 * and more faithful: the handler now receives a genuine `PointerEvent` whose
 * `clientX` is the real, read-only attribute.
 *
 * `bubbles` and `cancelable` reproduce what test-utils used for these three
 * types (`dom-event-types`), so nothing about propagation changes.
 */
export async function firePointer(
  wrapper: { element: Element },
  type: 'pointerdown' | 'pointermove' | 'pointerup',
  init: PointerEventInit = {},
): Promise<void> {
  wrapper.element.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, ...init }))
  await nextTick()
}
