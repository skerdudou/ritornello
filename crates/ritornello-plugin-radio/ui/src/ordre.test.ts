import { describe, expect, it } from 'vitest'
import { deplacer } from './ordre'

describe('deplacer', () => {
  it('deplace vers le bas et vers le haut', () => {
    expect(deplacer(['A', 'B', 'C'], 0, 2)).toEqual(['B', 'C', 'A'])
    expect(deplacer(['A', 'B', 'C'], 2, 0)).toEqual(['C', 'A', 'B'])
    expect(deplacer(['A', 'B', 'C'], 1, 2)).toEqual(['A', 'C', 'B'])
  })

  it('ne modifie pas la liste d origine', () => {
    // La liste est une `ref` Vue : la muter en place contournerait la
    // reactivite sur certains chemins et rendrait le test des composants
    // dependant de l'ordre des assertions.
    const origine = ['A', 'B', 'C']
    deplacer(origine, 0, 2)
    expect(origine).toEqual(['A', 'B', 'C'])
  })

  it('rend la liste inchangee sur un deplacement impossible', () => {
    // Les indices viennent d'evenements de glisser-deposer du navigateur : une
    // cible peut disparaitre entre le `dragstart` et le `drop`.
    expect(deplacer(['A', 'B'], 0, 0)).toEqual(['A', 'B'])
    expect(deplacer(['A', 'B'], -1, 1)).toEqual(['A', 'B'])
    expect(deplacer(['A', 'B'], 0, 5)).toEqual(['A', 'B'])
    expect(deplacer(['A', 'B'], 9, 0)).toEqual(['A', 'B'])
    expect(deplacer([], 0, 0)).toEqual([])
    expect(deplacer(['A', 'B'], 0.5, 1)).toEqual(['A', 'B'])
  })
})
