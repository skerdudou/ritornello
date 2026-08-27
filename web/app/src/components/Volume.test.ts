import { Slider } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import Volume from './Volume.vue'

const monte = (props: Record<string, unknown> = {}) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Volume, { props: { volume: 60, muted: false, desactive: false, ...props } })
}

describe('Volume', () => {
  beforeAll(() => {
    Element.prototype.setPointerCapture ??= () => {}
    Element.prototype.releasePointerCapture ??= () => {}
    Element.prototype.hasPointerCapture ??= () => true
    // jsdom ne fournit pas ResizeObserver ; reka-ui l'utilise pour mesurer la
    // piste du curseur au montage (voir BarreProgression.test.ts).
    globalThis.ResizeObserver ??= class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  })

  it('affiche la valeur et une poignée à cette valeur', async () => {
    const w = monte()
    // reka-ui resout aria-valuenow un tick apres le montage sous jsdom.
    await flushPromises()
    expect(w.get('[data-volume]').text()).toBe('60 %')
    expect(w.get('[role="slider"]').attributes('aria-valuenow')).toBe('60')
  })

  it('n’affiche rien avant la première trame', () => {
    const w = monte({ volume: null })
    expect(w.get('[data-volume]').text()).toBe('')
  })

  it('valide un réglage absolu au relâchement, une seule fois', async () => {
    const w = monte()
    const poignee = w.get('[role="slider"]')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('regler')).toEqual([[61]])
    expect(w.get('[data-volume]').text()).toBe('61 %')
  })

  it('le haut-parleur est la bascule Muet, et dit son état', async () => {
    // Demandé à l'usage sur l'ancienne page : on lisait « Volume : 60 % » sans
    // comprendre pourquoi rien ne sortait. Ici le muet barre la valeur et
    // change l'icône, au seul endroit où l'on cherche le son.
    const w = monte({ muted: true })
    const bouton = w.get('[data-remote-command="Mute"]')
    expect(bouton.attributes('aria-pressed')).toBe('true')
    expect(bouton.attributes('data-actif')).toBe('true')
    expect(w.get('[data-volume]').classes()).toContain('line-through')
    await bouton.trigger('click')
    expect(w.emitted('muet')).toHaveLength(1)
  })

  it('le geste suit le doigt localement et ne valide qu au relachement', async () => {
    // Un seul `regler` par geste : pendant le glisser, seul l'affichage bouge.
    // Emission directe sur le composant (comme dans BarreProgression.test.ts)
    // plutot que de vrais pointerdown/move/up, sous jsdom sans mise en page reelle.
    const w = monte()
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('update:modelValue', [25])
    expect(w.emitted('regler')).toBeUndefined()
    expect(w.get('[data-volume]').text()).toBe('25 %')
    await slider.vm.$emit('valueCommit', [25])
    expect(w.emitted('regler')).toEqual([[25]])
  })

  it('la valeur visee tient jusqu a la trame qui la rejoint', async () => {
    // Sans cela, la trame suivante (volume d'avant le reglage) ramenait la
    // poignee en arriere un instant — le meme defaut que sur BarreProgression.
    const w = monte()
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('valueCommit', [25])
    expect(w.emitted('regler')).toEqual([[25]])
    await w.setProps({ volume: 60 }) // la trame d'avant le reglage
    expect(w.get('[data-volume]').text()).toBe('25 %')
    await w.setProps({ volume: 25 }) // la trame qui confirme
    expect(w.get('[data-volume]').text()).toBe('25 %')
  })

  it('un volume change ailleurs (telecommande infrarouge) relache la valeur visee', async () => {
    // La page valide 40, puis quelqu'un d'autre (telecommande IR) touche
    // encore au volume : 41, 42, 43... La trame ne retombe jamais sur la
    // valeur visee (25 ici), donc l'egalite stricte seule laisserait
    // l'affichage fige — mais toute trame differente de la precedente prouve
    // que l'appareil a parle et doit relacher la visee.
    const w = monte()
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('valueCommit', [25])
    expect(w.emitted('regler')).toEqual([[25]])
    // Trame en vol, encore la valeur d'avant le reglage : ne relache rien.
    await w.setProps({ volume: 60 })
    expect(w.get('[data-volume]').text()).toBe('25 %')
    // L'appareil parle enfin, mais pas avec la valeur visee : il faut quand
    // meme relacher, sinon la page resterait figee sur "25 %" pour toujours.
    await w.setProps({ volume: 61 })
    expect(w.get('[data-volume]').text()).toBe('61 %')
  })

  it('en veille, curseur et bascule sont grisés', () => {
    const w = monte({ desactive: true })
    expect(w.get('[data-remote-command="Mute"]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-slot="slider"]').attributes('data-disabled')).toBeDefined()
  })
})
