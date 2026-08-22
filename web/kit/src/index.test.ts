import { flushPromises, mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { Button, cn, Tabs, TabsContent, TabsList, TabsTrigger, UI_CONTRACT } from './index'

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

  it('les onglets ne montent que le panneau actif, et le clic en change', async () => {
    // Le démontage des panneaux inactifs n'est pas un détail d'affichage :
    // une page qui range des volets dans des onglets cesse de faire vivre ce
    // qu'on ne regarde pas. Autant que ce soit vérifié ici plutôt que
    // découvert par un sondage qui s'arrête tout seul.
    const w = mount(
      {
        components: { Tabs, TabsList, TabsTrigger, TabsContent },
        template: `
          <Tabs default-value="a">
            <TabsList>
              <TabsTrigger value="a">Un</TabsTrigger>
              <TabsTrigger value="b">Deux</TabsTrigger>
            </TabsList>
            <TabsContent value="a">panneau A</TabsContent>
            <TabsContent value="b">panneau B</TabsContent>
          </Tabs>`,
      },
      { attachTo: document.body },
    )
    await flushPromises()
    expect(w.text()).toContain('panneau A')
    expect(w.text()).not.toContain('panneau B')

    // Le focus **puis** le clic : reka-ui active l'onglet au focus (mode
    // « automatic »), ce qu'un vrai clic produit toujours mais que
    // `trigger('click')` seul ne fait pas sous jsdom.
    const second = w.findAll('[data-slot="tabs-trigger"]')[1]!
    ;(second.element as HTMLElement).focus()
    await second.trigger('click')
    await flushPromises()
    expect(w.text()).toContain('panneau B')
    expect(w.text()).not.toContain('panneau A')
    w.unmount()
  })
})
