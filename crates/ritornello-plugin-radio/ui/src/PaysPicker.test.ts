import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import PaysPicker from './PaysPicker.vue'

const LISTE = [
  { code: 'DE', stations: 6081 },
  { code: 'FR', stations: 2746 },
  { code: 'BE', stations: 300 },
]

function monter(current = '') {
  return mount(PaysPicker, {
    props: {
      liste: LISTE,
      current,
      labelTous: 'Tous les pays',
      placeholder: 'Pays ou code',
      vide: 'Aucun pays',
    },
  })
}

describe('PaysPicker', () => {
  it('liste les pays par nom lisible, et non par code', () => {
    // Le composant emploie la langue du navigateur ; sous jsdom c'est `en-US`,
    // donc Belgium / France / Germany. L'ordre obtenu (BE, FR, DE) differe de
    // l'ordre des codes (DE, FR, BE) **et** de l'ordre d'entree : c'est
    // exactement ce qu'on veut verifier ici. Le tri par langue explicite est
    // couvert dans `pays.test.ts`.
    const w = monter()
    const codes = w.findAll('[data-country]').map((b) => b.attributes('data-country'))
    expect(codes[0]).toBe('ALL')
    expect(codes.slice(1)).toEqual(['BE', 'FR', 'DE'])
    expect(w.find('[data-country="FR"]').text()).toContain('2746')
  })

  it('filtre a la frappe', async () => {
    const w = monter()
    await w.find('[data-country-filter]').setValue('belg')
    const codes = w.findAll('[data-country]').map((b) => b.attributes('data-country'))
    expect(codes).toEqual(['ALL', 'BE'])
  })

  it('garde « tous les pays » atteignable quel que soit le filtre', async () => {
    // C'est le moyen de revenir en arriere : le filtrer serait un piege.
    const w = monter('FR')
    await w.find('[data-country-filter]').setValue('zzzz')
    expect(w.find('[data-country="ALL"]').exists()).toBe(true)
    expect(w.find('[data-country-empty]').text()).toBe('Aucun pays')
  })

  it('marque le pays courant', () => {
    const w = monter('BE')
    expect(w.find('[data-country="BE"]').attributes('data-active')).toBe('true')
    expect(w.find('[data-country="FR"]').attributes('data-active')).toBe('false')
    expect(w.find('[data-country="ALL"]').attributes('data-active')).toBe('false')
  })

  it('marque « tous les pays » quand aucun pays n est choisi', () => {
    expect(monter('').find('[data-country="ALL"]').attributes('data-active')).toBe('true')
  })

  it('emet le code choisi, et la chaine vide pour « tous »', async () => {
    const w = monter()
    await w.find('[data-country="FR"]').trigger('click')
    await w.find('[data-country="ALL"]').trigger('click')
    expect(w.emitted('choose')).toEqual([['FR'], ['']])
  })

  it('reste utilisable quand la liste est vide', () => {
    // Annuaire injoignable : le selecteur ne doit pas disparaitre, « tous les
    // pays » reste choisissable.
    const w = mount(PaysPicker, {
      props: { liste: [], current: 'FR', labelTous: 'Tous', placeholder: 'p', vide: 'Aucun pays' },
    })
    expect(w.find('[data-country="ALL"]').exists()).toBe(true)
    expect(w.find('[data-country-empty]').exists()).toBe(true)
  })
})
