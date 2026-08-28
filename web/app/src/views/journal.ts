import type { DateFormat } from '../types'

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


const deuxChiffres = (n: number) => String(n).padStart(2, '0')

/**
 * Une date écrite comme l'appareil est réglé pour l'écrire.
 *
 * Un choix fermé et non `Intl` : le rendu d'`Intl` dépend de la locale du
 * navigateur *et* du moteur, donc il ne se teste pas de façon stable et il
 * contredirait le réglage — c'est précisément ce réglage-là qui décide, pas la
 * machine de qui regarde.
 */
export function formateDate(d: Date, format: DateFormat): string {
  const annee = d.getFullYear()
  const mois = deuxChiffres(d.getMonth() + 1)
  const jour = deuxChiffres(d.getDate())
  if (format === 'year_month_day') return `${annee}-${mois}-${jour}`
  if (format === 'month_day_year') return `${mois}/${jour}/${annee}`
  return `${jour}/${mois}/${annee}`
}

/**
 * Une heure de journal : avec les secondes, contrairement à l'horloge de
 * veille. Deux lignes émises dans la même minute sont banales dans un journal,
 * et l'ordre y est l'information principale.
 *
 * Sur 12 h, minuit s'écrit `12:00:00 AM` et midi `12:00:00 PM` — la même
 * convention que l'afficheur console, et pour la même raison : `0:00 AM`
 * n'existe nulle part.
 */
export function formateHeure(d: Date, sur24h: boolean): string {
  const minutes = deuxChiffres(d.getMinutes())
  const secondes = deuxChiffres(d.getSeconds())
  if (sur24h) return `${deuxChiffres(d.getHours())}:${minutes}:${secondes}`
  const h = d.getHours()
  const suffixe = h < 12 ? 'AM' : 'PM'
  return `${h % 12 === 0 ? 12 : h % 12}:${minutes}:${secondes} ${suffixe}`
}

/**
 * Réécrit l'horodatage en tête d'une ligne de journal dans le format réglé, et
 * **dans le fuseau du navigateur**.
 *
 * Le cœur journalise en UTC (`2026-08-28T12:18:32.016060Z`), ce qui est le bon
 * choix pour un fichier mais se lit mal quand on cherche « ce qui s'est passé
 * il y a cinq minutes ». Le fuseau vient du navigateur et non d'un réglage :
 * c'est celui de qui regarde, ce qui reste juste pour un téléphone qui voyage,
 * là où un réglage de plus pourrait contredire l'appareil.
 *
 * Une ligne sans horodatage reconnaissable est rendue **telle quelle** : le
 * tampon du cœur ne contient aujourd'hui que ses propres lignes, mais une
 * ligne qu'on ne sait pas lire doit rester lisible plutôt que d'être tronquée.
 */
export function dateeLigne(ligne: string, format: DateFormat, sur24h: boolean): string {
  const trouve = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)\s+/.exec(ligne)
  if (!trouve) return ligne
  const d = new Date(trouve[1]!)
  if (Number.isNaN(d.getTime())) return ligne
  return `${formateDate(d, format)} ${formateHeure(d, sur24h)} ${ligne.slice(trouve[0].length)}`
}
