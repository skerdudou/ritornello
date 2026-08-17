import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import BarreProgression from './BarreProgression.vue'

const monte = (props: Record<string, unknown>) =>
  mount(BarreProgression, { props: { position: 87, duree: 254, deplacable: false, pas: 10, ...props } })

describe('BarreProgression', () => {
  it('affiche la position et la duree', () => {
    const w = monte({})
    expect(w.get('[data-position]').text()).toBe('1:27')
    expect(w.get('[data-duree-totale]').text()).toBe('4:14')
  })

  // Une barre sans fin n'apprend rien : sans duree, seul l'ecoule s'affiche.
  it('sans duree, pas de barre', () => {
    const w = monte({ duree: null })
    expect(w.find('[data-barre]').exists()).toBe(false)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('remplit la barre au prorata', () => {
    const w = monte({})
    expect(w.get('[data-remplissage]').attributes('style')).toContain('34')
  })

  // C'est `deplacable` qui decide, pas la presence d'une duree : Radio France
  // annonce une duree sur un direct qu'on ne peut pas rembobiner.
  it('inerte quand le contenu n est pas deplacable', async () => {
    const w = monte({ deplacable: false })
    await w.get('[data-barre]').trigger('click')
    expect(w.emitted('deplacer')).toBeUndefined()
    expect(w.get('[data-barre]').attributes('role')).toBeUndefined()
  })

  it('emet la seconde visee au clic', async () => {
    const w = monte({ deplacable: true })
    const barre = w.get('[data-barre]')
    barre.element.getBoundingClientRect = () =>
      ({ left: 0, width: 200, top: 0, height: 4, right: 200, bottom: 4, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect
    await barre.trigger('click', { clientX: 100 })
    expect(w.emitted('deplacer')?.[0]).toEqual([127])
  })

  // Sans le clavier, la barre serait la seule commande de la page hors
  // d'atteinte sans souris, sur une page dont toutes les autres sont des
  // boutons.
  it('se pilote au clavier', async () => {
    const w = monte({ deplacable: true })
    const barre = w.get('[data-barre]')
    expect(barre.attributes('role')).toBe('slider')
    expect(barre.attributes('tabindex')).toBe('0')
    await barre.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('deplacer')?.[0]).toEqual([97])
    await barre.trigger('keydown', { key: 'ArrowLeft' })
    expect(w.emitted('deplacer')?.[1]).toEqual([77])
    await barre.trigger('keydown', { key: 'Home' })
    expect(w.emitted('deplacer')?.[2]).toEqual([0])
    await barre.trigger('keydown', { key: 'End' })
    expect(w.emitted('deplacer')?.[3]).toEqual([254])
  })
})
