import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import ThemePicker from './ThemePicker.vue'

describe('ThemePicker', () => {
  it('propagates the catalog placeholder onto the rendered search field', () => {
    // The placeholder now comes from the catalog (key `theme_filter`, embedded
    // English value "filter" — the one the Playwright journey targets via
    // `getByPlaceholder('filter')`). Without a loaded catalog, `t()` falls
    // back to the key: that is what we must see go all the way down to the
    // `<input>` rendered by the kit's `Input` component.
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    expect(w.find('input').attributes('placeholder')).toBe('theme_filter')
  })

  it('lists the 42 themes with four swatches each', () => {
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    expect(w.findAll('[data-preset]')).toHaveLength(42)
    const card = w.find('[data-preset="northern-lights"]')
    expect(card.findAll('[data-swatch]')).toHaveLength(4)
  })

  it('marks the active theme', () => {
    const w = mount(ThemePicker, { props: { current: 'vercel', mode: 'light' } })
    expect(w.find('[data-preset="vercel"]').attributes('data-active')).toBe('true')
    expect(w.find('[data-preset="northern-lights"]').attributes('data-active')).toBe('false')
  })

  it('emits the chosen preset', async () => {
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    await w.find('[data-preset="vercel"]').trigger('click')
    expect(w.emitted('choose')).toEqual([['vercel']])
  })

  it('the swatches follow the displayed mode', () => {
    const light = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    const dark = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'dark' } })
    const background = (w: ReturnType<typeof mount>) =>
      w.find('[data-preset="northern-lights"] [data-swatch="background"]').attributes('style')
    expect(background(light)).toContain('rgb(249, 249, 250)')
    expect(background(dark)).toContain('rgb(26, 29, 35)')
  })
})
