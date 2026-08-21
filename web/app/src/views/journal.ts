/**
 * Lignes de journal retenues par une requête de filtre : sous-chaîne,
 * insensible à la casse, ordre d'entrée préservé.
 *
 * L'ordre est un contrat, pas un hasard : `/api/logs` rend les lignes les plus
 * récentes en premier, et un filtre qui retrierait renverserait cette
 * chronologie sans que l'appelant l'ait demandé.
 *
 * Une requête vide — ou faite d'espaces seuls, ce qu'un champ de saisie produit
 * en permanence — rend la liste entière plutôt qu'aucune ligne : un champ qu'on
 * vient de vider doit rendre ce qu'on voyait avant d'y taper.
 */
export function filtreLignes(lignes: string[], requete: string): string[] {
  const q = requete.trim().toLowerCase()
  if (!q) return lignes
  return lignes.filter((l) => l.toLowerCase().includes(q))
}
