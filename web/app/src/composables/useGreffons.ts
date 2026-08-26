import { api } from '@ritornello/ui'
import { computed, ref } from 'vue'
import type { StatusPayload } from '../types'

/**
 * L'état des greffons, au niveau **module** — un seul jeu d'état pour toute la
 * SPA, comme le catalogue de `useCatalog` et les métriques de `useMetriques`.
 *
 * Ce n'est pas un rangement mais la correction d'un défaut. Cet état vivait en
 * deux copies : un `ref` local dans `App.vue` pour les entrées de menu des
 * pages d'admin, un autre dans `ConfigView.vue` pour le tableau. Celui de la
 * navigation était lu **une seule fois**, au montage de la SPA, et plus jamais.
 * Deux symptômes s'ensuivaient, qui n'en faisaient qu'un :
 *
 * - désactiver un greffon laissait son entrée dans le menu du haut, et un clic
 *   menait à une page d'admin qui n'existait plus (le cœur retire le dorsal, la
 *   route répond 404) ;
 * - le rallumer laissait la ligne sur « figé » indéfiniment, alors que le cœur
 *   la remplace dès l'annonce du greffon. Un F5 était le seul recours, pour les
 *   deux.
 *
 * Les entrées de menu se déduisent donc de la même source que le tableau, et
 * cette source se relit — voir `surveille`.
 */

const etat = ref<StatusPayload>({ plugins: [], active_source: '' })

/**
 * `/api/status` injoignable. Distingué d'un état vide : la navigation sans
 * aucun plugin admin est le symptôme le plus difficile à attribuer, la page
 * ayant l'air normale par ailleurs.
 */
const indisponible = ref(false)

/**
 * Période de relecture pendant qu'un greffon est « figé ».
 *
 * Un sondage, et non un flux poussé : le cœur n'expose pas de SSE sur l'état
 * des greffons, et lui en ajouter un pour une fenêtre de quelques secondes
 * coûterait plus que ce qu'il rapporte. Ce qui rend le sondage acceptable sur
 * un Raspberry Pi 2, c'est qu'il **ne tourne pas en régime établi** : il ne
 * s'arme que tant qu'une ligne dit « figé », c'est-à-dire pendant la fenêtre
 * entre le lancement d'un binaire et son annonce.
 */
const PERIODE_MS = 1500

/**
 * Plafond de relectures, soit 30 s.
 *
 * Sans lui, un greffon lancé qui **n'annonce jamais** — un état que le cœur
 * nomme lui-même (`Gathered::figes`) et qui décrit un greffon fautif, pas un
 * greffon lent — ferait sonder la page jusqu'à sa fermeture. Le cœur ne laisse
 * que 5 s à une connexion pour écrire sa ligne d'annonce ; 30 s couvrent donc
 * le lancement du processus avec une marge large, et au-delà la ligne « figé »
 * n'est plus une attente mais un diagnostic, qui doit rester affiché tel quel.
 */
const TENTATIVES_MAX = 20

let minuteur: ReturnType<typeof setTimeout> | null = null
let restantes = 0

/** Noms des greffons joignables qui ont une page d'admin, dédoublonnés.
 *
 * Une ligne de statut par (nom, genre) : un greffon multi-genres avec page
 * d'admin (ex. `mpd` en `input` + `display`) pousse plusieurs lignes portant le
 * même `admin: true`. Sans le `Set`, la nav afficherait autant de liens
 * identiques que de genres — voir la même clé `${name}-${kind}` dans
 * `ConfigView.vue` pour le tableau, qui lui doit garder les doublons.
 *
 * Aucun filtre sur `disabled` ni sur `stalled`, et ce n'est pas un oubli : le
 * cœur remplace **toutes** les lignes d'un greffon éteint par une seule
 * `desactive()`, et celles d'un greffon relancé par une `genre_inconnu()`, qui
 * portent l'une comme l'autre `admin: false`. Un `admin: true` prouve donc à
 * lui seul que le greffon s'est annoncé et que son dorsal est câblé. Ajouter
 * `&& !p.disabled` serait une garde dont la fausseté ne se verrait jamais.
 */
const admins = computed(() => [
  ...new Set(etat.value.plugins.filter((p) => p.admin).map((p) => p.name)),
])

/** Y a-t-il un greffon lancé qui n'a pas encore parlé ?
 *
 * **Les deux états**, et c'est le piège de cette relecture : depuis qu'un
 * greffon fraîchement rallumé est rapporté « démarrage » et non plus « figé »,
 * ne surveiller que `stalled` aurait désarmé le sondage pendant exactement la
 * fenêtre pour laquelle il existe — celle où la ligne va être remplacée par
 * l'annonce. Le rallumage serait redevenu invisible sans F5, le défaut d'avant.
 */
const enAttente = () => etat.value.plugins.some((p) => p.stalled || p.starting)

/**
 * Relit `/api/status`. Sur échec, l'état précédent est **conservé** : une
 * coupure passagère ne doit pas vider le menu ni le tableau.
 */
async function recharger(): Promise<void> {
  const s = await api.get<StatusPayload>('/api/status').catch((e) => {
    console.warn('GET /api/status indisponible : navigation sans les plugins admin', e)
    return null
  })
  indisponible.value = s === null
  if (s) etat.value = s
}

/**
 * Arme la relecture tant qu'un greffon est figé.
 *
 * Appelé après chaque bascule et après chaque relecture. Le minuteur est au
 * niveau module et remis à zéro à chaque armement : deux appelants — la nav et
 * la page de configuration — ne peuvent pas faire tourner deux boucles
 * concurrentes sur la même donnée.
 *
 * Le compteur repart à plein à chaque appel **venu de l'extérieur** (une
 * bascule), et seulement décroît sur les tours de la boucle. C'est ce qui fait
 * qu'un greffon fautif finit par cesser d'être sondé, alors qu'un second clic
 * de l'utilisateur redonne toujours sa chance à la surveillance.
 */
function surveille(): void {
  if (minuteur !== null) {
    clearTimeout(minuteur)
    minuteur = null
  }
  restantes = TENTATIVES_MAX
  boucle()
}

function boucle(): void {
  if (!enAttente() || restantes <= 0) {
    minuteur = null
    return
  }
  restantes -= 1
  minuteur = setTimeout(async () => {
    await recharger()
    boucle()
  }, PERIODE_MS)
}

/**
 * Relit l'état, puis surveille la fenêtre « figé » qu'un rallumage vient
 * d'ouvrir. C'est l'unique point d'entrée : l'amorçage de la SPA et le
 * rafraîchissement d'après-bascule font exactement la même chose, et leur
 * donner deux noms n'aurait décrit qu'une intention, pas une différence.
 *
 * Contrairement à `useMetriques().demarrer()`, plusieurs appelants sont sûrs :
 * `surveille` désarme le minuteur en cours avant d'en poser un autre, donc deux
 * boucles ne peuvent pas se disputer la même donnée. C'est ce qui permet à
 * `App.vue` d'amorcer et à `ConfigView` de rafraîchir sans se coordonner.
 */
async function rafraichir(): Promise<void> {
  await recharger()
  surveille()
}

/**
 * Aucun export de remise a zero pour les tests : cet etat vit au niveau module,
 * et les tests repartent d'un module frais par `vi.resetModules()` — le meme
 * motif que `useCatalog.test.ts`. Un `_reinitialise()` exporte serait du code de
 * production existant pour les seuls tests, et une seconde facon de vider
 * l'etat qu'il faudrait garder d'accord avec celle-ci.
 */
export function useGreffons() {
  return { etat, indisponible, admins, rafraichir }
}
