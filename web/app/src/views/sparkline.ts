/**
 * Abscisses des échantillons dans le repère du graphe, proportionnelles à leur
 * **horodatage** et non à leur rang.
 *
 * Des points équidistants supposent une cadence constante. Elle ne l'est pas :
 * la période de sondage se change en cours de route, et l'historique garde
 * alors des échantillons espacés de 30 s à côté d'autres espacés de 1 s. Les
 * répartir également mentirait sur le temps — cinq minutes d'histoire ancienne
 * occuperaient autant de largeur que cinq secondes récentes, et la pente d'une
 * montée de charge n'aurait plus de sens. En partant des horodatages, l'axe
 * redevient un axe de temps, et le tracé reconverge de lui-même vers
 * l'équidistance à mesure que les anciens échantillons sortent du tampon.
 *
 * Le premier échantillon est à 0 et le dernier à `largeur` : la fenêtre est
 * toujours pleine, c'est son échelle qui change. Voir `dureeFenetreMin`, qui
 * annonce la durée réellement couverte, mesurée sur les mêmes horodatages.
 *
 * Étendue nulle — deux échantillons dans la même milliseconde, ou une horloge
 * figée par des minuteurs simulés — : repli sur l'équidistance, qui vaut mieux
 * qu'une division par zéro.
 */
export function abscisses(horodatages: number[], largeur: number): number[] {
  const n = horodatages.length
  if (n < 2) return n === 1 ? [0] : []
  const debut = horodatages[0]
  const etendue = horodatages[n - 1] - debut
  if (etendue <= 0) return horodatages.map((_, i) => (i * largeur) / (n - 1))
  return horodatages.map((t) => ((t - debut) / etendue) * largeur)
}

/**
 * Construit l'attribut `d` d'un `<path>` SVG pour une série de pourcentages,
 * placée aux abscisses fournies par `abscisses`.
 *
 * Toute la géométrie du graphe tient ici et dans `abscisses`, en fonctions
 * pures et testées : la vue n'a plus qu'à passer ses deux séries. Les
 * abscisses arrivent en paramètre plutôt que d'être recalculées ici parce que
 * les deux séries, le trait de survol et le calage du popin doivent partager
 * exactement les mêmes — un popin décalé d'une colonne par rapport au tracé
 * qu'il commente serait pire qu'une absence de popin.
 *
 * Les valeurs sont bornées à 0-100 — une charge supérieure au nombre de
 * cœurs dépasse 100 % et ne doit pas sortir du cadre — et l'axe y est
 * inversé : 0 % en bas, comme on lit un graphe, alors que le repère SVG a
 * son origine en haut.
 *
 * Moins de deux points : chaîne vide. Un échantillon seul ne dessine pas de
 * ligne, et un `d` vide est un `<path>` invisible, pas une erreur. Autant
 * d'abscisses que de valeurs, sinon chaîne vide également : un appel mal
 * apparié dessinerait des `NaN`, une dégradation silencieuse vaut mieux.
 */
export function cheminSparkline(valeurs: number[], xs: number[], hauteur: number): string {
  if (valeurs.length < 2 || xs.length !== valeurs.length) return ''
  return valeurs
    .map((v, i) => {
      const borne = Math.min(100, Math.max(0, v))
      const y = hauteur - (borne / 100) * hauteur
      return `${i === 0 ? 'M' : 'L'}${xs[i].toFixed(2)},${y.toFixed(2)}`
    })
    .join(' ')
}
