/**
 * Nom lisible d'une langue, dans **sa propre langue** : `fr` donne
 * « francais », `en` donne « English ».
 *
 * C'est la convention des selecteurs de langue, et la seule qui se lise quand on
 * ne comprend pas celle qui est active : quelqu'un qui tombe sur un appareil
 * regle en francais doit pouvoir trouver « English » sans savoir lire le
 * francais.
 *
 * Les noms ne sont pas traduits par nos packs : le coeur n'expose que des codes
 * (les noms de fichiers `<lang>.toml`), et `Intl.DisplayNames` les rend depuis
 * les donnees du navigateur — rien a tenir a jour de notre cote quand un pack de
 * langue est ajoute.
 *
 * Repli sur le code : une langue inconnue du moteur doit rester selectionnable,
 * pas disparaitre du selecteur.
 */
export function nomLangue(code: string): string {
  const brut = code.trim()
  if (!brut) return ''
  try {
    const noms = new Intl.DisplayNames([brut], { type: 'language' })
    const nom = noms.of(brut)
    // Capitalise seulement un **nom** : quand `Intl` ne connait pas le code, il
    // le renvoie tel quel, et un code s'affiche verbatim — « Qqq » ne serait ni
    // un nom ni un code.
    return nom && nom !== brut ? majuscule(nom, brut) : brut
  } catch {
    return brut
  }
}

/**
 * Premiere lettre en majuscule.
 *
 * Les conventions typographiques divergent — l'anglais capitalise les noms de
 * langue (« English »), le francais non (« francais ») — et une liste ou les
 * entrees alternent les deux se lit mal. On capitalise donc toutes les entrees.
 *
 * `toLocaleUpperCase` avec la langue concernee, et non `toUpperCase` : la
 * transformation depend de la langue (le turc distingue `i` et `ı`), et elle est
 * sans effet sur les ecritures qui n'ont pas de casse.
 */
function majuscule(nom: string, langue: string): string {
  const premiere = nom.slice(0, 1).toLocaleUpperCase(langue)
  return premiere + nom.slice(1)
}
