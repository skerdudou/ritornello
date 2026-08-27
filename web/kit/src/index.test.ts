import { flushPromises, mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { Button, cn, Slider, Tabs, TabsContent, TabsList, TabsTrigger, UI_CONTRACT } from './index'

// jsdom ne fournit pas ResizeObserver ; reka-ui l'utilise (`useSize`) pour
// mesurer la piste du curseur au montage. Un stub minimal suffit : le test
// ne verifie aucune position au pixel pres.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
vi.stubGlobal('ResizeObserver', ResizeObserverStub)

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

  it('le curseur rend une poignée accessible et valide un pas de clavier', async () => {
    // Un seul composant pour la progression et le volume : ce qui est vérifié
    // ici — la poignée est un `role=slider`, un pas de clavier émet la valeur
    // **et** la valide — est ce que les deux usages supposent.
    const w = mount(Slider, {
      props: { modelValue: [60], min: 0, max: 100, step: 1, 'aria-label': 'Volume' },
      attachTo: document.body,
    })
    await flushPromises()
    const poignee = w.get('[role="slider"]')
    expect(poignee.attributes('aria-valuenow')).toBe('60')
    expect(poignee.attributes('aria-valuemin')).toBe('0')
    expect(poignee.attributes('aria-valuemax')).toBe('100')
    // Sans le tri des attrs dans Slider.vue, `aria-label` file vers le
    // `<span>` englobant de `SliderRoot` et la poignée reste sans nom
    // accessible : c'est elle, pas le root, que verifie ce test.
    expect(poignee.attributes('aria-label')).toBe('Volume')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('update:modelValue')?.[0]).toEqual([[61]])
    expect(w.emitted('valueCommit')?.[0]).toEqual([[61]])
    // « part une fois » : un seul pas clavier ne doit pas produire plusieurs
    // validations.
    expect(w.emitted('valueCommit')).toHaveLength(1)
    w.unmount()
  })
})
