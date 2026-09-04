import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { Skeleton } from './index'

describe('Skeleton', () => {
  it('is skipped by assistive technology', () => {
    // A screen reader has nothing to gain from grey rectangles: what it must
    // hear during a wait is one message, announced by the container (see the
    // `role="status"` of `PluginView`), not one node per fake line.
    const w = mount(Skeleton)
    expect(w.attributes('aria-hidden')).toBe('true')
    expect(w.attributes('data-slot')).toBe('skeleton')
  })

  it('keeps the size the caller asked for', () => {
    // The whole point of the component is that the caller gives it the
    // dimensions of what it stands in for. A class silently dropped here
    // would collapse every placeholder to the same height — exactly the
    // layout jump this work removes.
    const w = mount(Skeleton, { props: { class: 'h-8 w-40' } })
    expect(w.classes()).toContain('h-8')
    expect(w.classes()).toContain('w-40')
  })

  it('pulses only where motion is welcome', () => {
    // jsdom computes no style, so the class list is the only observable: the
    // `motion-safe:` variant compiles to a `prefers-reduced-motion:
    // no-preference` query, hence a still placeholder for a reader who asked
    // the system for less movement.
    const w = mount(Skeleton)
    expect(w.classes()).toContain('motion-safe:animate-pulse')
    expect(w.classes()).not.toContain('animate-pulse')
  })
})
