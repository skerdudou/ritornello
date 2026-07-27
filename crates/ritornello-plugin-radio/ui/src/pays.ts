// Pays de l'annuaire : logique pure, sans Vue et sans reseau, donc testable
// telle quelle.
//
// Le plugin ne transporte que des **codes ISO** (voir `DirectoryCountry`) : le
// nom lisible est rendu ici par `Intl.DisplayNames`, donc dans la langue du
// navigateur et sans table de 241 pays a tenir a jour ni a traduire de notre
// cote. C'est aussi ce qui evite le defaut qu'avait la version precedente, ou
// le libelle affiche venait d'une cle de traduction resolue trop tot.

export interface Pays {
  code: string
  stations: number
}

/** Un pays pret a afficher : code, nom lisible, nombre de stations. */
export interface PaysAffichable extends Pays {
  nom: string
}

/** Code du choix « tous les pays » : c'est ce que le plugin attend (`country: ''`). */
export const TOUS_PAYS = ''

/**
 * Langue a employer pour les noms de pays.
 *
 * Celle du navigateur, et non celle de l'appareil : le catalogue transmis aux
 * IHM de plugin ne porte pas le code de langue, et l'ajouter au contrat pour ce
 * seul usage serait disproportionne. Consequence assumee : un navigateur en
 * anglais affichera « Germany » sur un appareil en francais.
 */
function langueNavigateur(): string {
  return (typeof navigator !== 'undefined' && navigator.language) || 'en'
}

/**
 * Nom lisible d'un code ISO. Repli sur le code lui-meme : un code inconnu du
 * moteur (ou un moteur sans `Intl.DisplayNames`) doit rester selectionnable,
 * pas disparaitre de la liste.
 */
export function nomPays(code: string, langue: string = langueNavigateur()): string {
  const brut = code.trim().toUpperCase()
  if (!brut) return ''
  try {
    const noms = new Intl.DisplayNames([langue], { type: 'region' })
    return noms.of(brut) ?? brut
  } catch {
    return brut
  }
}

/** Retire accents et casse, pour que « etats » trouve « États-Unis ». */
function plie(s: string): string {
  return s
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
}

/**
 * Liste a afficher : filtree sur le nom **ou** le code, puis triee par nom.
 *
 * Le tri est fait sur le nom lisible et non sur le code : « Allemagne » se
 * cherche a la lettre A, pas a DE. Le filtre accepte aussi le code, parce que
 * c'est ce qu'on tape quand on le connait.
 */
export function paysAffichables(
  liste: Pays[],
  filtre = '',
  langue: string = langueNavigateur(),
): PaysAffichable[] {
  const f = plie(filtre.trim())
  return liste
    .map((p) => ({ ...p, nom: nomPays(p.code, langue) }))
    .filter((p) => !f || plie(p.nom).includes(f) || plie(p.code).includes(f))
    .sort((a, b) => a.nom.localeCompare(b.nom, langue))
}
