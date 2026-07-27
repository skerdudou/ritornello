// Reordonnancement des stations : logique pure, testable sans DOM.
//
// La preselection **est** la position de la ligne (voir `enregistrer()` dans
// RadioAdmin.vue et `save_numerote_de_1_a_n_par_position` cote plugin) : glisser
// une station change donc son numero de telecommande, et c'est bien l'intention.

/**
 * Deplace l'element d'index `de` a l'index `vers`, en renvoyant une **nouvelle**
 * liste.
 *
 * Un index hors bornes ou un deplacement sur place rend la liste inchangee
 * plutot que de lever : les indices viennent d'evenements de glisser-deposer du
 * navigateur, ou une cible peut disparaitre entre le `dragstart` et le `drop`.
 */
export function deplacer<T>(liste: readonly T[], de: number, vers: number): T[] {
  const copie = [...liste]
  if (
    !Number.isInteger(de) ||
    !Number.isInteger(vers) ||
    de < 0 ||
    vers < 0 ||
    de >= copie.length ||
    vers >= copie.length ||
    de === vers
  ) {
    return copie
  }
  const [element] = copie.splice(de, 1)
  // `element` ne peut pas etre `undefined` ici (index verifie ci-dessus), mais
  // le type de `splice` ne le sait pas.
  copie.splice(vers, 0, element as T)
  return copie
}
