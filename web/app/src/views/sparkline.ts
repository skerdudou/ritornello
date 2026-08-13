/**
 * Construit l'attribut `d` d'un `<path>` SVG pour une série de pourcentages.
 *
 * Toute la géométrie du graphe tient ici, en fonction pure et testée : la
 * vue n'a plus qu'à passer ses deux séries.
 *
 * Les valeurs sont bornées à 0-100 — une charge supérieure au nombre de
 * cœurs dépasse 100 % et ne doit pas sortir du cadre — et l'axe y est
 * inversé : 0 % en bas, comme on lit un graphe, alors que le repère SVG a
 * son origine en haut.
 *
 * Moins de deux points : chaîne vide. Un échantillon seul ne dessine pas de
 * ligne, et un `d` vide est un `<path>` invisible, pas une erreur.
 */
export function cheminSparkline(valeurs: number[], largeur: number, hauteur: number): string {
  if (valeurs.length < 2) return ''
  const pas = largeur / (valeurs.length - 1)
  return valeurs
    .map((v, i) => {
      const borne = Math.min(100, Math.max(0, v))
      const x = i * pas
      const y = hauteur - (borne / 100) * hauteur
      return `${i === 0 ? 'M' : 'L'}${x.toFixed(2)},${y.toFixed(2)}`
    })
    .join(' ')
}
