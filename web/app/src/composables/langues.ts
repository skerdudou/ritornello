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
    return noms.of(brut) ?? brut
  } catch {
    return brut
  }
}
