/**
 * Abonnement aux changements d'état du player, poussés par le cœur.
 *
 * Existe dans le kit — et non recopié dans chaque plugin — parce que les pages
 * de plugin en ont le même besoin que le shell : savoir *quand* quelque chose a
 * changé pour relire ce qui les concerne. Sans cela, une page qui affiche la
 * piste en cours ne peut que sonder, et le projet a déjà tranché contre le
 * sondage (voir `usePlayer` du shell : « le cœur pousse déjà chaque
 * changement »).
 *
 * La charge utile est passée **non typée**, à dessein : sa forme appartient au
 * cœur et changera sans prévenir. Un appelant qui n'a besoin que du signal
 * l'ignore ; celui qui a besoin d'un champ précis (la source active, par
 * exemple) le lit à ses risques, sans qu'on figé ici un type qui mentirait.
 */
export function onPlayer(rappel: (etat: unknown) => void): () => void {
  // `EventSource` n'existe pas partout (jsdom sous test, vieux moteurs) : son
  // absence doit coûter la fraîcheur de l'affichage, jamais le rendu de la page.
  if (typeof EventSource === 'undefined') return () => {}

  const flux = new EventSource('/api/player')
  flux.onmessage = (e: MessageEvent) => {
    try {
      rappel(JSON.parse(e.data as string))
    } catch {
      // Trame illisible : le signal vaut quand même, l'appelant relira sa
      // propre source de vérité.
      rappel(null)
    }
  }
  // Aucun traitement d'erreur : `EventSource` se reconnecte de lui-même, et
  // fermer ici priverait la page de toute reprise après un redémarrage du cœur
  // — le cas le plus courant étant `systemctl restart ritornello`.
  return () => flux.close()
}
