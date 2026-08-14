import { describe, expect, it } from 'vitest'
import { abscisses, cheminSparkline } from './sparkline'

describe('abscisses', () => {
  it('ancre le premier point à 0 et le dernier au bord', () => {
    expect(abscisses([0, 5000], 100)).toEqual([0, 100])
  })

  it('espace les points selon le temps écoulé et non selon leur rang', () => {
    // Le cas que le placement par rang rendait faux : trois échantillons, le
    // second pris 2 s après le premier, le troisième 6 s plus tard. Il
    // appartient au premier quart du temps couvert, donc au premier quart de
    // la largeur — un placement par rang l'aurait mis au milieu.
    expect(abscisses([0, 2000, 8000], 100)).toEqual([0, 25, 100])
  })

  it('reconverge vers l équidistance quand les écarts redeviennent réguliers', () => {
    // C'est ce qui se produit à mesure que les échantillons pris à l'ancienne
    // période sortent du tampon : plus rien ne distingue le graphe d'un tracé
    // équidistant.
    expect(abscisses([0, 1000, 2000, 3000, 4000], 100)).toEqual([0, 25, 50, 75, 100])
  })

  it('retombe sur l équidistance quand l étendue est nulle', () => {
    // Plusieurs échantillons dans la même milliseconde (ou une horloge figée
    // par des minuteurs simulés) : équidistant faute de mieux, et surtout pas
    // une division par zéro qui remplirait le tracé de `NaN`.
    expect(abscisses([7, 7, 7], 100)).toEqual([0, 50, 100])
  })

  it('rend une liste vide ou un unique zéro sous deux points', () => {
    expect(abscisses([], 100)).toEqual([])
    expect(abscisses([42], 100)).toEqual([0])
  })
})

describe('cheminSparkline', () => {
  it('ne dessine rien avec moins de deux points', () => {
    expect(cheminSparkline([], [], 30)).toBe('')
    expect(cheminSparkline([42], [0], 30)).toBe('')
  })

  it('ne dessine rien si les abscisses ne sont pas appariées aux valeurs', () => {
    // Un appel mal apparié dessinerait des `NaN` : mieux vaut un `<path>`
    // invisible.
    expect(cheminSparkline([0, 50, 100], [0, 100], 30)).toBe('')
  })

  it('inverse l axe y : 0 % en bas, 100 % en haut', () => {
    expect(cheminSparkline([0, 100], [0, 100], 30)).toBe('M0.00,30.00 L100.00,0.00')
  })

  it('borne les valeurs hors de 0-100', () => {
    // Une charge supérieure au nombre de cœurs dépasse 100 % et ne doit pas
    // sortir du cadre.
    expect(cheminSparkline([-10, 200], [0, 100], 30)).toBe(
      cheminSparkline([0, 100], [0, 100], 30),
    )
  })

  it('suit les abscisses fournies plutôt que de répartir également', () => {
    // Les mêmes trois valeurs, placées d'abord régulièrement puis selon des
    // horodatages irréguliers : seule l'abscisse du point médian change.
    expect(cheminSparkline([0, 50, 100], [0, 50, 100], 30)).toBe(
      'M0.00,30.00 L50.00,15.00 L100.00,0.00',
    )
    expect(cheminSparkline([0, 50, 100], abscisses([0, 2000, 8000], 100), 30)).toBe(
      'M0.00,30.00 L25.00,15.00 L100.00,0.00',
    )
  })
})
