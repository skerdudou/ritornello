import { Slider } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it } from 'vitest'
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
    const style = w.get('[data-remplissage]').attributes('style') ?? ''
    const pourcent = Number(/width:\s*([\d.]+)%/.exec(style)?.[1])
    // 87 / 254 = 34,25 %. Une valeur lue et comparee, plutot qu'une
    // sous-chaine « 34 » qui passerait aussi bien sur « 3.4 » ou « 340 ».
    expect(pourcent).toBeCloseTo(34.25, 1)
  })

  // C'est `deplacable` qui decide, pas la presence d'une duree : Radio France
  // annonce une duree sur un direct qu'on ne peut pas rembobiner.
  it('inerte quand le contenu n est pas deplacable', async () => {
    const w = monte({ deplacable: false })
    await w.get('[data-barre]').trigger('click')
    expect(w.emitted('deplacer')).toBeUndefined()
    expect(w.get('[data-barre]').attributes('role')).toBeUndefined()
    expect(w.find('[role="slider"]').exists()).toBe(false)
    // La barre statique n'est pas une cible : elle ne doit pas payer la zone
    // de contact de 44 px (`py-[19px]`) reservee au vrai curseur, et doit
    // partager l'exacte meme geometrie que le curseur (`py-0` des deux
    // cotes, mesure Playwright a l'appui : radio et fichier doivent
    // s'aligner).
    const classes = w.get('[data-barre]').classes()
    expect(classes).not.toContain('py-[19px]')
    expect(classes).toContain('py-0')
  })

  // Peu importe l'etat (barre statique ou curseur) : la ligne des durees
  // reste sous la piste dans le DOM, jamais avant. Une regression possible
  // avec des marges negatives (`-my-[19px]`, `-mt-4`) qui pourraient a tort
  // faire chevaucher ou reordonner les blocs visuellement.
  it('la ligne des durees reste sous la piste, non deplacable', () => {
    const w = monte({ deplacable: false })
    const piste = w.get('[data-barre]').element
    const durees = w.get('[data-position]').element.closest('div')!
    expect(piste.compareDocumentPosition(durees) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  })

  it('la ligne des durees reste sous la piste, deplacable', async () => {
    const w = monte({ deplacable: true })
    await flushPromises()
    const piste = w.get('[data-slot="slider"]').element
    const durees = w.get('[data-position]').element.closest('div')!
    expect(piste.compareDocumentPosition(durees) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  })

  // reka-ui capture le pointeur pendant le glisser ; jsdom n'implemente pas
  // cette API. Trois cales, le temps du fichier.
  beforeAll(() => {
    Element.prototype.setPointerCapture ??= () => {}
    Element.prototype.releasePointerCapture ??= () => {}
    Element.prototype.hasPointerCapture ??= () => true
    // jsdom ne fournit pas ResizeObserver ; reka-ui l'utilise pour mesurer la
    // piste du curseur au montage (voir web/kit/src/index.test.ts).
    globalThis.ResizeObserver ??= class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  })

  function rectangle(w: ReturnType<typeof monte>) {
    const piste = w.get('[data-slot="slider"]')
    piste.element.getBoundingClientRect = () =>
      ({ left: 0, width: 200, top: 0, height: 44, right: 200, bottom: 44, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect
    return piste
  }

  it('un contenu deplacable rend une poignee accessible', async () => {
    // `await flushPromises()` : reka-ui resout l'index de la poignee depuis
    // la collection des thumbs montes (`SliderThumb`), remplie a l'accroche
    // de la ref DOM. Au premier rendu, l'index vaut -1 (poignee pas encore
    // dans la collection) et `aria-valuenow` reste absent ; un tick plus tard
    // il vaut 0 et l'attribut apparait. Constate ici, meme cause que le stub
    // ResizeObserver ci-dessus (mesure/collecte differee sous jsdom).
    const w = monte({ deplacable: true })
    await flushPromises()
    const poignee = w.get('[role="slider"]')
    expect(poignee.attributes('aria-valuenow')).toBe('87')
    expect(poignee.attributes('aria-valuemax')).toBe('254')
  })

  it('le glisser suit le doigt localement et ne valide qu au relachement', async () => {
    // Un seul `SeekTo` par geste : pendant le glisser, seul l'affichage bouge.
    //
    // Repli sur les evenements du composant plutot que de vrais
    // pointerdown/move/up : sous jsdom, `thumb.clientWidth` vaut toujours 0
    // (pas de mise en page reelle), donc reka calcule le geste sur la largeur
    // pleine de la piste (200 px). 150/200 x 254 tombe exactement sur 190,5 s,
    // que `Math.round` de reka arrondit a 191 et non 190 — un desaccord de
    // mesure propre a jsdom, pas un defaut du composant (verifie en inspectant
    // `SliderHorizontal.getValueFromPointerEvent`). Dans un navigateur reel, la
    // poignee a une largeur non nulle et ne tombe pas sur cette frontiere ; le
    // geste reel est couvert par l'e2e (Tache 12).
    const w = monte({ deplacable: true })
    const slider = w.getComponent(Slider)
    await slider.vm.$emit('update:modelValue', [190])
    expect(w.emitted('deplacer')).toBeUndefined()
    expect(w.get('[data-position]').text()).toBe('3:10') // 150/200 × 254 = 190 s, affiché pendant le geste
    await slider.vm.$emit('valueCommit', [190])
    expect(w.emitted('deplacer')).toEqual([[190]])
  })

  it('la valeur visee tient jusqu a la trame qui la rejoint', async () => {
    // Sans cela, la trame suivante (position d'avant le saut) ramenait la
    // poignée en arrière un instant — le défaut visible des lecteurs naïfs.
    const w = monte({ deplacable: true })
    const piste = rectangle(w)
    await piste.trigger('pointerdown', { clientX: 100, pointerId: 1, button: 0 })
    await piste.trigger('pointerup', { clientX: 100, pointerId: 1 })
    expect(w.emitted('deplacer')).toEqual([[127]])
    await w.setProps({ position: 88 }) // la trame d'avant le saut
    expect(w.get('[data-position]').text()).toBe('2:07')
    await w.setProps({ position: 129 }) // à un pas près : on la rejoint
    expect(w.get('[data-position]').text()).toBe('2:09')
  })

  it('une trame sans position relache la valeur visee au lieu de la figer', async () => {
    // Fin de piste, Stop, veille, changement de source : aucune de ces trames
    // ne porte de position, et aucune ne viendra jamais confirmer le saut —
    // sans quoi la barre resterait bloquee sur l'ancienne cible pour toujours.
    const w = monte({ deplacable: true, position: 87 })
    const piste = rectangle(w)
    await piste.trigger('pointerdown', { clientX: 100, pointerId: 1, button: 0 })
    await piste.trigger('pointerup', { clientX: 100, pointerId: 1 })
    expect(w.emitted('deplacer')).toEqual([[127]])
    await w.setProps({ position: null })
    // Plus de position : rien n'est rendu (voir le test « affiche la position
    // et la duree »), la valeur visee ne doit donc laisser aucune trace.
    expect(w.find('[data-progression]').exists()).toBe(false)
    // Une piste suivante qui repart a 0:01 le prouve : sans le relachement,
    // la visee (127) aurait encore masque cette valeur toute neuve.
    await w.setProps({ position: 1 })
    expect(w.get('[data-position]').text()).toBe('0:01')
  })

  // Sans le clavier, la barre serait la seule commande de la page hors
  // d'atteinte sans souris. Le pas est celui des touches physiques
  // (`seek_step_s`), pas la seconde du curseur.
  it('le clavier deplace du pas configure, borne aux deux bouts', async () => {
    const w = monte({ deplacable: true, position: 250 })
    const poignee = w.get('[role="slider"]')
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('deplacer')?.[0]).toEqual([254])
    await poignee.trigger('keydown', { key: 'Home' })
    expect(w.emitted('deplacer')?.[1]).toEqual([0])
    await poignee.trigger('keydown', { key: 'ArrowLeft' })
    expect(w.emitted('deplacer')?.[2]).toEqual([240])
  })
})
