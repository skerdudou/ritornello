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
  const debut = horodatages[0]
  const fin = horodatages[n - 1]
  // Indéfinis ⇔ tableau vide. Le test porte sur les valeurs et non sur `n`,
  // parce que `noUncheckedIndexedAccess` ne déduit pas d'un `n >= 2` que les
  // accès indexés sont sûrs — et une assertion `!` masquerait ici la seule
  // chose qui rende ce code total.
  if (debut === undefined || fin === undefined) return []
  if (n === 1) return [0]
  const etendue = fin - debut
  if (etendue <= 0) return horodatages.map((_, i) => (i * largeur) / (n - 1))
  return horodatages.map((t) => ((t - debut) / etendue) * largeur)
}

const MINUTE_MS = 60_000

/**
 * Nombre maximum de repères rendus. La fenêtre réelle plafonne bien en dessous
 * (240 échantillons à 30 s font 120 minutes), mais un horodatage aberrant — une
 * horloge qui saute, chose banale sur une machine sans pile ni réseau au
 * démarrage — produirait sinon des milliers d'éléments pour un graphe large de
 * quelques centaines de pixels.
 */
const MAX_REPERES = 240

/**
 * Abscisses des repères de minute : une marque à chaque **minute pleine de
 * l'horloge** (secondes à zéro) tombant dans la fenêtre couverte, rendues de
 * gauche à droite.
 *
 * Des minutes de l'horloge et non un décompte depuis « maintenant » : une
 * marque désigne alors un instant réel, celui qu'on lit sur une montre, et deux
 * captures d'écran prises à des moments différents parlent du même axe. La
 * contrepartie est que les marques **glissent** vers la gauche à mesure que le
 * temps passe, au lieu de rester immobiles — c'est le propre d'un instant fixe
 * sur une fenêtre qui défile, pas un défaut.
 *
 * Le modulo suffit à trouver ces instants : l'époque Unix tombe elle-même sur
 * une minute pleine et tous les décalages horaires usuels sont des multiples
 * de la minute, donc `t % 60000 == 0` vaut « secondes à zéro » quelle que soit
 * la zone.
 *
 * Même échelle que `abscisses`, forcément : un repère qui ne partagerait pas
 * l'échelle du tracé désignerait un autre instant que celui qu'il prétend.
 * Étendue nulle : aucune marque. Une fenêtre plus courte qu'une minute peut au
 * contraire en porter une, si une minute pleine tombe dedans.
 */
export function reperesMinute(horodatages: number[], largeur: number): number[] {
  const n = horodatages.length
  const debut = horodatages[0]
  const fin = horodatages[n - 1]
  if (debut === undefined || fin === undefined) return []
  const etendue = fin - debut
  if (etendue <= 0) return []
  const premier = Math.ceil(debut / MINUTE_MS) * MINUTE_MS
  const combien = Math.min(
    Math.floor((fin - premier) / MINUTE_MS) + 1,
    MAX_REPERES,
  )
  if (combien <= 0) return []
  return Array.from(
    { length: combien },
    (_, i) => ((premier + i * MINUTE_MS - debut) / etendue) * largeur,
  )
}

/**
 * Construit l'attribut `d` d'un `<path>` SVG pour une série de pourcentages,
 * placée aux abscisses fournies par `abscisses`. Une valeur `null` marque un
 * échantillon dont la mesure a échoué (par exemple une température illisible)
 * : elle ouvre un **trou** dans le tracé plutôt que d'être comblée.
 *
 * Toute la géométrie du graphe tient ici et dans `abscisses`, en fonctions
 * pures et testées : la vue n'a plus qu'à passer ses séries. Les
 * abscisses arrivent en paramètre plutôt que d'être recalculées ici parce que
 * les trois séries, le trait de survol et le calage du popin doivent partager
 * exactement les mêmes — un popin décalé d'une colonne par rapport au tracé
 * qu'il commente serait pire qu'une absence de popin.
 *
 * Les valeurs présentes sont bornées à 0-100 — une charge supérieure au
 * nombre de cœurs dépasse 100 % et ne doit pas sortir du cadre — et l'axe y
 * est inversé : 0 % en bas, comme on lit un graphe, alors que le repère SVG a
 * son origine en haut.
 *
 * Un `<path>` SVG accepte plusieurs sous-tracés : chaque `null` referme le
 * sous-tracé courant, et l'échantillon présent suivant en rouvre un avec un
 * nouveau `M` plutôt que de poursuivre avec un `L`. Deux points de part et
 * d'autre du trou ne sont donc jamais reliés par un trait — la seule autre
 * option praticable serait de recopier la dernière valeur connue sur le trou,
 * ce qui dessinerait un plateau parfaitement horizontal, indiscernable à l'œil
 * d'une mesure réelle et stable. Un trou visible dit « on ne sait pas » ; un
 * plateau prétendrait le savoir.
 *
 * Moins de deux points : chaîne vide. Un échantillon seul ne dessine pas de
 * ligne, et un `d` vide est un `<path>` invisible, pas une erreur — c'est
 * aussi ce qui se produit pour un sous-tracé d'un seul point isolé entre deux
 * trous : un `M` sans `L` qui le suit, qui ne trace rien non plus, sans que ce
 * soit un cas à part. Autant d'abscisses que de valeurs, sinon chaîne vide
 * également : un appel mal apparié dessinerait des `NaN`, une dégradation
 * silencieuse vaut mieux. Toutes les valeurs `null` : chaîne vide aussi,
 * aucun sous-tracé ne s'ouvre jamais — le cas d'une machine sans la sonde
 * correspondante.
 */
export function cheminSparkline(
  valeurs: (number | null)[],
  xs: number[],
  hauteur: number,
): string {
  if (valeurs.length < 2 || xs.length !== valeurs.length) return ''
  let segment = true
  return valeurs
    .map((v, i) => {
      if (v === null) {
        // Referme le sous-tracé courant : le prochain point présent rouvrira
        // avec un `M`, pas un `L` qui le relierait par-dessus le trou.
        segment = true
        return ''
      }
      const borne = Math.min(100, Math.max(0, v))
      const y = hauteur - (borne / 100) * hauteur
      // Le `?? 0` est inatteignable — les deux longueurs viennent d'être
      // vérifiées égales — mais il vaut mieux qu'une assertion `!` : si
      // quelqu'un relâche un jour ce contrôle, le tracé se décale au lieu de
      // se remplir de `NaN`.
      const x = xs[i] ?? 0
      const commande = segment ? 'M' : 'L'
      segment = false
      return `${commande}${x.toFixed(2)},${y.toFixed(2)}`
    })
    .filter((s) => s !== '')
    .join(' ')
}
