/**
 * Abonnement aux changements d'état du lecteur, poussés par le cœur.
 *
 * Existe dans le kit — et non recopié dans chaque plugin — parce que les pages
 * de plugin en ont le même besoin que le shell : savoir *quand* quelque chose a
 * changé pour relire ce qui les concerne. Sans cela, une page qui affiche la
 * piste en cours ne peut que sonder, et le projet a déjà tranché contre le
 * sondage (voir `usePlayer` du shell : « le cœur pousse déjà chaque
 * changement »).
 *
 * Volontairement **sans charge utile** : le rappel dit « quelque chose a
 * bougé », à charge pour l'appelant de relire sa propre source de vérité. Cela
 * évite de dupliquer ici le type de l'état du lecteur, qui appartient au cœur et
 * changera sans prévenir.
 */
export function surLecteur(rappel: () => void): () => void {
  // `EventSource` n'existe pas partout (jsdom sous test, vieux moteurs) : son
  // absence doit coûter la fraîcheur de l'affichage, jamais le rendu de la page.
  if (typeof EventSource === 'undefined') return () => {}

  const flux = new EventSource('/api/player')
  flux.onmessage = () => rappel()
  // Aucun traitement d'erreur : `EventSource` se reconnecte de lui-même, et
  // fermer ici priverait la page de toute reprise après un redémarrage du cœur
  // — le cas le plus courant étant `systemctl restart ritornello`.
  return () => flux.close()
}
