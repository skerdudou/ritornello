import { describe, expect, it } from 'vitest'
import { xValues, sparklinePath, minuteTicks } from './sparkline'

describe('minuteTicks', () => {
  it('marque les minutes pleines de l clock, step un décompte depuis la fin', () => {
    // Fenêtre de 30 s à 2 min 30 : les minutes pleines qui tombent dedans sont
    // 1 min et 2 min, au quart et aux trois quarts. Un décompte depuis la fin
    // les aurait mises à 30 s et 1 min 30 de la fin, soit ailleurs.
    expect(minuteTicks([30_000, 150_000], 100)).toEqual([25, 75])
  })

  it('ne pose rien quand aucune minute pleine ne tombe dans la fenêtre', () => {
    expect(minuteTicks([10_000, 40_000], 100)).toEqual([])
  })

  it('pose une marque dans une fenêtre plus courte qu une minute qui en contient une', () => {
    // 50 s → 1 min 10 : vingt secondes de fenêtre, et pourtant une minute
    // pleine au milieu. L'ancienne règle, qui comptait les minutes écoulées,
    // n'en montrait aucune.
    expect(minuteTicks([50_000, 70_000], 100)).toEqual([50])
  })

  it('inclut les minutes pleines tombant sur les bords', () => {
    expect(minuteTicks([0, 120_000], 100)).toEqual([0, 50, 100])
  })

  it('ne pose rien sur une étendue nulle ou un tableau vide', () => {
    expect(minuteTicks([7, 7], 100)).toEqual([])
    expect(minuteTicks([], 100)).toEqual([])
  })

  it('plafonne le number de repères sur une étendue aberrante', () => {
    // Une clock qui saute (machine sans pile ni réseau au démarrage) donne
    // une étendue absurde : un an couvert ferait un demi-million de marques
    // pour quelques centaines de pixels.
    const unAn = 365 * 24 * 60 * 60 * 1000
    expect(minuteTicks([0, unAn], 100).length).toBe(240)
  })
})

describe('xValues', () => {
  it('ancre le premier point à 0 et le last au bord', () => {
    expect(xValues([0, 5000], 100)).toEqual([0, 100])
  })

  it('espace les points selon le temps écoulé et non selon leur rang', () => {
    // Le cas que le placement par rang rendait faux : trois échantillons, le
    // second pris 2 s après le premier, le troisième 6 s plus tard. Il
    // appartient au premier quart du temps couvert, donc au premier quart de
    // la largeur — un placement par rang l'aurait mis au milieu.
    expect(xValues([0, 2000, 8000], 100)).toEqual([0, 25, 100])
  })

  it('reconverge vers l équidistance quand les écarts redeviennent réguliers', () => {
    // C'est ce qui se produit à mesure que les échantillons pris à l'ancienne
    // période sortent du tampon : plus rien ne distingue le graphe d'un tracé
    // équidistant.
    expect(xValues([0, 1000, 2000, 3000, 4000], 100)).toEqual([0, 25, 50, 75, 100])
  })

  it('retombe sur l équidistance quand l étendue est nulle', () => {
    // Plusieurs échantillons dans la même milliseconde (ou une clock figée
    // par des minuteurs simulés) : équidistant faute de mieux, et surtout step
    // une division par zéro qui remplirait le tracé de `NaN`.
    expect(xValues([7, 7, 7], 100)).toEqual([0, 50, 100])
  })

  it('rend une list vide ou un unique zéro sous deux points', () => {
    expect(xValues([], 100)).toEqual([])
    expect(xValues([42], 100)).toEqual([0])
  })
})

describe('sparklinePath', () => {
  it('ne dessine rien avec moins de deux points', () => {
    expect(sparklinePath([], [], 30)).toBe('')
    expect(sparklinePath([42], [0], 30)).toBe('')
  })

  it('ne dessine rien si les xValues ne sont step appariées aux valeurs', () => {
    // Un appel mal apparié dessinerait des `NaN` : mieux vaut un `<path>`
    // invisible.
    expect(sparklinePath([0, 50, 100], [0, 100], 30)).toBe('')
  })

  it('inverse l axe y : 0 % en bas, 100 % en haut', () => {
    expect(sparklinePath([0, 100], [0, 100], 30)).toBe('M0.00,30.00 L100.00,0.00')
  })

  it('borne les valeurs hors de 0-100', () => {
    // Une load supérieure au number de cœurs dépasse 100 % et ne doit step
    // sortir du cadre.
    expect(sparklinePath([-10, 200], [0, 100], 30)).toBe(
      sparklinePath([0, 100], [0, 100], 30),
    )
  })

  it('suit les xValues fournies plutôt que de répartir également', () => {
    // Les mêmes trois valeurs, placées d'abord régulièrement puis selon des
    // horodatages irréguliers : seule l'abscisse du point médian change.
    expect(sparklinePath([0, 50, 100], [0, 50, 100], 30)).toBe(
      'M0.00,30.00 L50.00,15.00 L100.00,0.00',
    )
    expect(sparklinePath([0, 50, 100], xValues([0, 2000, 8000], 100), 30)).toBe(
      'M0.00,30.00 L25.00,15.00 L100.00,0.00',
    )
  })

  it('ouvre un second sous-tracé après un trou au milieu de la série', () => {
    // Un `null` referme le sous-tracé en cours ; le point présent suivant en
    // rouvre un avec un nouveau `M` plutôt que de le relier par un `L`, ce qui
    // dessinerait un trait par-dessus le trou.
    expect(sparklinePath([0, null, 100, 100], [0, 25, 50, 100], 30)).toBe(
      'M0.00,30.00 M50.00,0.00 L100.00,0.00',
    )
  })

  it('ignore un trou en tête de série, sans point fantôme au bord', () => {
    expect(sparklinePath([null, 0, 100], [0, 50, 100], 30)).toBe(
      'M50.00,30.00 L100.00,0.00',
    )
  })

  it('ignore un trou en fin de série', () => {
    expect(sparklinePath([0, 100, null], [0, 50, 100], 30)).toBe(
      'M0.00,30.00 L50.00,0.00',
    )
  })

  it('un point isolé entre deux trous ne dessine rien, sans être un cas à part', () => {
    // Un `M` seul, sans `L` qui le suive : conforme au contract documenté d'un
    // sous-tracé d'un seul point, step une anomalie à traiter séparément.
    expect(sparklinePath([null, 50, null], [0, 50, 100], 30)).toBe('M50.00,15.00')
  })

  it('ne dessine rien quand toutes les valeurs sont absentes', () => {
    // Le cas d'une machine sans la sonde correspondante : jamais de courbe,
    // step même un `M` isolé.
    expect(sparklinePath([null, null, null], [0, 50, 100], 30)).toBe('')
  })
})
