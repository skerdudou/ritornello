import { describe, expect, it } from 'vitest'
import { abscisses, cheminSparkline, reperesMinute } from './sparkline'

describe('reperesMinute', () => {
  it('marque les minutes pleines de l horloge, pas un décompte depuis la fin', () => {
    // Fenêtre de 30 s à 2 min 30 : les minutes pleines qui tombent dedans sont
    // 1 min et 2 min, au quart et aux trois quarts. Un décompte depuis la fin
    // les aurait mises à 30 s et 1 min 30 de la fin, soit ailleurs.
    expect(reperesMinute([30_000, 150_000], 100)).toEqual([25, 75])
  })

  it('ne pose rien quand aucune minute pleine ne tombe dans la fenêtre', () => {
    expect(reperesMinute([10_000, 40_000], 100)).toEqual([])
  })

  it('pose une marque dans une fenêtre plus courte qu une minute qui en contient une', () => {
    // 50 s → 1 min 10 : vingt secondes de fenêtre, et pourtant une minute
    // pleine au milieu. L'ancienne règle, qui comptait les minutes écoulées,
    // n'en montrait aucune.
    expect(reperesMinute([50_000, 70_000], 100)).toEqual([50])
  })

  it('inclut les minutes pleines tombant sur les bords', () => {
    expect(reperesMinute([0, 120_000], 100)).toEqual([0, 50, 100])
  })

  it('ne pose rien sur une étendue nulle ou un tableau vide', () => {
    expect(reperesMinute([7, 7], 100)).toEqual([])
    expect(reperesMinute([], 100)).toEqual([])
  })

  it('plafonne le nombre de repères sur une étendue aberrante', () => {
    // Une horloge qui saute (machine sans pile ni réseau au démarrage) donne
    // une étendue absurde : un an couvert ferait un demi-million de marques
    // pour quelques centaines de pixels.
    const unAn = 365 * 24 * 60 * 60 * 1000
    expect(reperesMinute([0, unAn], 100).length).toBe(240)
  })
})

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
