import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import ThemePicker from './ThemePicker.vue'

describe('ThemePicker', () => {
  it('propage le placeholder du catalogue sur le champ de recherche rendu', () => {
    // Le placeholder vient désormais du catalogue (clé `theme_filter`, valeur
    // anglaise embarquée « filter » — celle que le journey Playwright cible
    // via `getByPlaceholder('filter')`). Sans catalogue chargé, `t()` retombe
    // sur la clé : c'est elle qu'on doit voir descendre jusqu'à l'`<input>`
    // rendu par le composant `Input` du kit.
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    expect(w.find('input').attributes('placeholder')).toBe('theme_filter')
  })

  it('list les 42 thèmes avec quatre pastilles chacun', () => {
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    expect(w.findAll('[data-preset]')).toHaveLength(42)
    const carte = w.find('[data-preset="northern-lights"]')
    expect(carte.findAll('[data-swatch]')).toHaveLength(4)
  })

  it('marque le thème actif', () => {
    const w = mount(ThemePicker, { props: { current: 'vercel', mode: 'light' } })
    expect(w.find('[data-preset="vercel"]').attributes('data-active')).toBe('true')
    expect(w.find('[data-preset="northern-lights"]').attributes('data-active')).toBe('false')
  })

  it('émet le preset choisi', async () => {
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    await w.find('[data-preset="vercel"]').trigger('click')
    expect(w.emitted('choose')).toEqual([['vercel']])
  })

  it('les pastilles suivent le mode affiché', () => {
    const clair = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    const sombre = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'dark' } })
    const fond = (w: ReturnType<typeof mount>) =>
      w.find('[data-preset="northern-lights"] [data-swatch="background"]').attributes('style')
    expect(fond(clair)).toContain('rgb(249, 249, 250)')
    expect(fond(sombre)).toContain('rgb(26, 29, 35)')
  })
})
