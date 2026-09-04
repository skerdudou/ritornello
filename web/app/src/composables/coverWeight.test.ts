import { describe, expect, it } from 'vitest'
import { BYTES_PER_PIXEL, predictedThumbnailBytes } from './coverWeight'

const KIO = 1024

describe('predictedThumbnailBytes', () => {
  // Les trois points de la table viennent du banc Rust
  // `cover::tests::the_weight_rule_of_a_thumbnail`, mesuré sur 78 pochettes
  // réelles en release. Ce test est ce qui empêche la table de dériver
  // silencieusement de la mesure qui la justifie.
  it('reproduces the measured p50 at the product defaults', () => {
    // 640 px, q85 : 98 Kio mesurés.
    expect(Math.round(predictedThumbnailBytes(640, 85) / KIO)).toBe(98)
  })

  it('follows the measured direction of quality', () => {
    // Mesuré : 73 Kio à q75, 98 à q85, 120 à q90. Le modèle doit être
    // monotone croissant en qualité — une table mal saisie qui inverserait
    // deux lignes ferait échouer ceci.
    const q75 = predictedThumbnailBytes(640, 75)
    const q85 = predictedThumbnailBytes(640, 85)
    const q90 = predictedThumbnailBytes(640, 90)
    expect(q75).toBeLessThan(q85)
    expect(q85).toBeLessThan(q90)
  })

  it('interpolates between two measured qualities', () => {
    const q80 = predictedThumbnailBytes(640, 80)
    expect(q80).toBeGreaterThan(predictedThumbnailBytes(640, 75))
    expect(q80).toBeLessThan(predictedThumbnailBytes(640, 85))
  })

  it('clamps outside the measured range instead of extrapolating', () => {
    // q40 est la borne basse du réglage et q100 la haute, or le banc n'a
    // mesuré ni l'une ni l'autre. Extrapoler linéairement donnerait une
    // densité négative en dessous de q≈53 — un poids négatif affiché à
    // l'utilisateur. Le bornage est ce qui l'interdit.
    expect(predictedThumbnailBytes(640, 40)).toBe(predictedThumbnailBytes(640, 75))
    expect(predictedThumbnailBytes(640, 100)).toBe(predictedThumbnailBytes(640, 90))
    expect(predictedThumbnailBytes(640, 40)).toBeGreaterThan(0)
  })

  it('grows with the square of the edge', () => {
    // Doubler le côté quadruple les pixels, donc le poids.
    const small = predictedThumbnailBytes(320, 85)
    const large = predictedThumbnailBytes(640, 85)
    expect(large / small).toBeCloseTo(4, 5)
  })

  it('returns zero on a value a number input can actually produce', () => {
    // Vider une boîte pour la retaper est une frappe ordinaire, et
    // `Number('')` vaut 0 — pas NaN. Sans ce cas, la page afficherait
    // « environ 0 Kio », et l'estimation diviserait par zéro.
    expect(predictedThumbnailBytes(0, 85)).toBe(0)
    expect(predictedThumbnailBytes(640, 0)).toBe(0)
    expect(predictedThumbnailBytes(Number.NaN, 85)).toBe(0)
    expect(predictedThumbnailBytes(-640, 85)).toBe(0)
  })

  it('keeps the table anchored to the bench', () => {
    // Le changement de production qui ferait échouer ceci : éditer la table
    // sans relancer le banc. Les trois couples sont ceux du rapport.
    expect(BYTES_PER_PIXEL).toEqual([
      [75, 0.18],
      [85, 0.245],
      [90, 0.3],
    ])
  })
})
