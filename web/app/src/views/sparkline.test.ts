import { describe, expect, it } from 'vitest'
import { cheminSparkline } from './sparkline'

describe('cheminSparkline', () => {
  it('ne dessine rien avec moins de deux points', () => {
    expect(cheminSparkline([], 100, 30)).toBe('')
    expect(cheminSparkline([42], 100, 30)).toBe('')
  })

  it('inverse l axe y : 0 % en bas, 100 % en haut', () => {
    expect(cheminSparkline([0, 100], 100, 30)).toBe('M0.00,30.00 L100.00,0.00')
  })

  it('borne les valeurs hors de 0-100', () => {
    // Une charge supérieure au nombre de cœurs dépasse 100 % et ne doit pas
    // sortir du cadre.
    expect(cheminSparkline([-10, 200], 100, 30)).toBe(cheminSparkline([0, 100], 100, 30))
  })

  it('repartit les points sur toute la largeur', () => {
    expect(cheminSparkline([0, 50, 100], 100, 30)).toBe('M0.00,30.00 L50.00,15.00 L100.00,0.00')
  })
})
