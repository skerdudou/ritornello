import type { Page } from '@playwright/test'

/**
 * What sticks out sideways, on a page or inside one container.
 *
 * Without `root`, the verdict is the document's: `width` is its client width,
 * `scroll` its scroll width, and the leaves examined are those under `body`.
 * With `root` (a selector, first match, typically a dialog), `width` is the
 * root's right edge and `scroll` the largest leaf right edge found.
 *
 * A leaf is a culprit when it ends past `width`, unless an ancestor below the
 * root clips it (a non-`visible` `overflow-x`) or, in page mode, is fixed. On
 * top of that, any element that scrolls sideways by itself is a culprit
 * (`… scrolls sideways`) unless it carries `data-scrolls-sideways`: an
 * intentional scroller must say so, with a comment explaining why.
 */
export async function sidewaysCulprits(
  page: Page,
  root?: string,
): Promise<{ scroll: number; width: number; culprits: string[] }> {
  return page.evaluate((rootSelector) => {
    const scope: HTMLElement | null = rootSelector
      ? document.querySelector<HTMLElement>(rootSelector)
      : document.body
    if (!scope) throw new Error(`no element matches ${rootSelector}`)
    const docEl = document.documentElement
    const width = rootSelector ? scope.getBoundingClientRect().right : docEl.clientWidth
    let widest = width
    const culprits: string[] = []
    const describe = (el: HTMLElement, what: string): string => {
      const marker = Array.from(el.attributes).find((a) => a.name.startsWith('data-'))?.name
      const text = (el.textContent ?? '').trim().slice(0, 40)
      return `${el.tagName.toLowerCase()}${marker ? `[${marker}]` : ''} "${text}" ${what}`
    }
    for (const el of Array.from(scope.querySelectorAll<HTMLElement>('*'))) {
      const style = getComputedStyle(el)
      if (
        (style.overflowX === 'auto' || style.overflowX === 'scroll') &&
        el.scrollWidth > el.clientWidth + 1 &&
        !el.hasAttribute('data-scrolls-sideways')
      ) {
        culprits.push(describe(el, 'scrolls sideways'))
      }
      if (el.children.length > 0) continue
      // Inside a container, a leaf whose content spills out of its own box
      // (a nowrap label wider than its grid cell) is a defect even while its
      // box stays within the container: it overprints its neighbour.
      if (rootSelector && style.overflowX === 'visible' && el.scrollWidth > el.clientWidth + 1) {
        culprits.push(describe(el, 'overflows its own box'))
      }
      const box = el.getBoundingClientRect()
      if (box.width === 0 || box.right <= width + 0.5) continue
      // A fixed element (the toaster's empty list) does not widen the page
      // either, whatever its box says; inside a dialog, fixed is the dialog.
      let clipped = false
      for (let p: HTMLElement | null = el; p && p !== scope; p = p.parentElement) {
        const s = getComputedStyle(p)
        if ((!rootSelector && s.position === 'fixed') || (p !== el && s.overflowX !== 'visible')) {
          clipped = true
          break
        }
      }
      if (clipped) continue
      widest = Math.max(widest, box.right)
      culprits.push(describe(el, `right=${Math.round(box.right)}`))
    }
    return { scroll: rootSelector ? widest : docEl.scrollWidth, width, culprits }
  }, root)
}
