import { describe, expect, it } from 'vitest'
import { move } from './order'

describe('move', () => {
  it('deplace vers le bas et vers le haut', () => {
    expect(move(['A', 'B', 'C'], 0, 2)).toEqual(['B', 'C', 'A'])
    expect(move(['A', 'B', 'C'], 2, 0)).toEqual(['C', 'A', 'B'])
    expect(move(['A', 'B', 'C'], 1, 2)).toEqual(['A', 'C', 'B'])
  })

  it('ne modifie pas la liste d origine', () => {
    // La liste est une `ref` Vue : la muter en place contournerait la
    // reactivite sur certains chemins et rendrait le test des composants
    // dependant de l'order des assertions.
    const origine = ['A', 'B', 'C']
    move(origine, 0, 2)
    expect(origine).toEqual(['A', 'B', 'C'])
  })

  it('rend la liste inchangee sur un deplacement impossible', () => {
    // Les indices viennent d'evenements de glisser-drop du navigateur : une
    // cible peut disparaitre entre le `dragstart` et le `drop`.
    expect(move(['A', 'B'], 0, 0)).toEqual(['A', 'B'])
    expect(move(['A', 'B'], -1, 1)).toEqual(['A', 'B'])
    expect(move(['A', 'B'], 0, 5)).toEqual(['A', 'B'])
    expect(move(['A', 'B'], 9, 0)).toEqual(['A', 'B'])
    expect(move([], 0, 0)).toEqual([])
    expect(move(['A', 'B'], 0.5, 1)).toEqual(['A', 'B'])
  })
})
