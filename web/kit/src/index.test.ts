import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { Button, cn, UI_CONTRACT } from './index'

describe('surface publique du kit', () => {
  it('expose la version de contrat', () => {
    expect(UI_CONTRACT).toBe(1)
  })

  it('cn fusionne les classes en respectant la dernière', () => {
    expect(cn('p-2', 'p-4')).toBe('p-4')
  })

  it('monte un Button avec son contenu', () => {
    const w = mount(Button, { slots: { default: 'Enregistrer' } })
    expect(w.text()).toBe('Enregistrer')
    expect(w.element.tagName).toBe('BUTTON')
  })
})
