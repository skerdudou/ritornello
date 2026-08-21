import { api } from '@ritornello/ui'
import { computed, ref } from 'vue'
import type { SystemPayload } from '../types'

/**
 * Sondage des métriques système et son historique, au niveau **module** — un
 * seul jeu d'état pour toute la SPA, comme le catalogue de `useCatalog`.
 *
 * Ce n'est pas un détail d'implémentation mais la raison d'être du fichier :
 * quand cet état vivait dans `SystemView.vue`, quitter la page pour la
 * configuration et revenir repartait d'un graphe vide, et l'historique ne
 * commençait à se remplir qu'à la première visite. Ici, `App.vue` l'amorce une
 * fois au montage de la SPA et il vit jusqu'à la fermeture de la page.
 *
 * Un seul point d'amorçage, donc : deux appelants qui démarreraient se
 * disputeraient le même minuteur.
 */

/**
 * Un point d'historique : deux pourcentages, une température en °C quand la
 * machine en expose une, et l'horodatage du sondage (qui porte l'axe des
 * abscisses, voir `abscisses` dans `views/sparkline.ts`).
 *
 * `temp` est nullable là où `cpu` et `ram` ne le sont pas, et c'est la
 * différence qui compte : une machine sans sonde garde son graphe, alors
 * qu'une machine dont la mémoire ou l'utilisation CPU est illisible n'a pas
 * d'échantillon du tout.
 */
export interface Echantillon { cpu: number; ram: number; temp: number | null; t: number }

const etat = ref<SystemPayload | null>(null)
const indisponible = ref(false)

/**
 * Période de sondage, au niveau module comme tout l'état de ce fichier : elle
 * vit donc autant que la page, et un choix fait sur l'onglet Système est encore
 * là quand on y revient — le contraire de la version locale à la vue, qui
 * repartait à 5 s à chaque arrivée. Elle n'est pas persistée pour autant, ni
 * dans `localStorage` ni dans `/api/settings` : c'est un confort de
 * visualisation, pas un réglage de l'appareil, et cette SPA ne garde aucun état
 * côté navigateur — ses préférences vivent dans le cœur. Un rechargement
 * complet la ramène donc à 5 s. Le `Select` ne porte que des chaînes (voir
 * `periode` ci-dessous) ; la valeur réelle en millisecondes vit ici pour
 * `setInterval`.
 */
const periodeMs = ref(5000)
/** Options du sélecteur de période, en secondes. */
export const PERIODES_S = [1, 2, 5, 10, 30] as const
/**
 * Nombre d'échantillons conservés — voir `dureeFenetreMin` pour la fenêtre
 * visible qui en découle à la période courante.
 *
 * 240 et non 60 : le sondage tourne désormais en continu, y compris onglet
 * caché et vue démontée, et une mémoire de 5 minutes à la période par défaut
 * ne rendrait presque rien de cette continuité. 240 échantillons font 20
 * minutes à 5 s, deux heures à 30 s. Le plafond est celui de la lisibilité,
 * pas du coût : 240 points sur un graphe large de quelques centaines de pixels
 * sont encore distinguables, quelques milliers ne le seraient plus.
 */
const CAPACITE = 240

const historique = ref<Echantillon[]>([])
/**
 * Sondage en vol, à double usage : `sonder()` s'en sert comme verrou pour
 * refuser de s'y superposer, `arreter()` pour l'annuler. Avant le delta CPU
 * stateful, une réponse en retard n'était qu'un affichage périmé ; désormais
 * une réponse qui atterrit dans le désordre écraserait `precedentJiffies`
 * avec une référence trop récente ou trop ancienne, et fausserait le delta
 * du sondage suivant (`Δtotal <= 0` ou une fenêtre bien plus longue que la
 * période affichée). D'où le verrou : un sondage déjà en vol en bloque un
 * second plutôt que de laisser deux réponses se doubler dans le désordre.
 */
let sondageEnVol: AbortController | null = null
let minuteur: ReturnType<typeof setInterval> | null = null
/**
 * Attente unique avant de reprendre le rythme, quand `demarrer()` constate que
 * l'échéance de la période courante n'est pas encore atteinte. Distincte de
 * `minuteur` parce qu'elle ne tique qu'une fois, et arrêtée par `arreter()`
 * comme lui — un `setTimeout` oublié rallumerait le sondage en pleine action
 * d'alimentation, ou doublerait le minuteur après un changement de période. Le
 * démontage d'une vue, lui, n'arrête plus rien : le sondage appartient au
 * module et survit à toutes les pages.
 */
let attente: ReturnType<typeof setTimeout> | null = null
/**
 * Horodatage du dernier sondage réellement lancé, qui datemarque l'échéance :
 * `dernierSondage + periodeMs` dit quand le prochain est dû. `null` tant
 * qu'aucun sondage n'a eu lieu — l'arrivée sur la page, où il n'y a rien à
 * attendre.
 */
let dernierSondage: number | null = null

/**
 * Compteurs jiffies du sondage précédent, pour calculer un delta — à part de
 * l'historique, qui n'a de sens qu'entre deux sondages consécutifs et non
 * comme une série à afficher.
 */
const precedentJiffies = ref<{ total: number; idle: number } | null>(null)

/**
 * Loquet du diagnostic console : vrai dès qu'un échec a été signalé, remis à
 * faux au premier succès qui suit.
 *
 * Le sondage tourne désormais en continu, depuis n'importe quelle page et
 * jusqu'à la fermeture de l'onglet : un cœur injoignable sans ce loquet
 * écrirait une ligne toutes les 5 s pour toujours — de l'ordre de 17 000
 * lignes par jour sur un onglet oublié avec un lien capricieux, chacune
 * retenant l'objet `Error` de sa requête dans l'historique de la console.
 * Avant ce store, l'avertissement ne courait que le temps où la page Système
 * était ouverte et visible, ce qui le bornait de fait.
 *
 * Le loquet ne dit donc qu'une chose : la *transition* vers l'échec, puis le
 * silence tant qu'il persiste. Réarmé au succès, une panne ultérieure
 * s'annonce à nouveau — c'est ce qui le distingue d'un simple « une fois pour
 * toute la vie de la page ». La ligne « indisponible » à l'écran, elle, reste
 * affichée en continu : c'est elle qui porte l'état, la console ne porte que
 * l'événement.
 */
let echecSignale = false

/** Dernière utilisation CPU calculée par `sonder`, indépendamment de
 *  l'historique : la carte CPU l'affiche dès qu'elle existe, sans attendre
 *  que la mémoire soit elle aussi lisible (condition propre à l'historique).
 *  Déclarée ici, à côté de `precedentJiffies` plutôt que près de son usage
 *  d'affichage plus bas : `sonder()` l'assigne, et ne compte que sur l'ordre
 *  d'exécution (le premier sondage part de l'amorçage du store dans `App.vue`,
 *  après l'évaluation du module) pour que ça reste sûr — un futur appel plus
 *  impatient tomberait sur la zone morte temporelle d'un `const` déclaré après
 *  coup. */
const utilisationCpuActuelle = ref<number | null>(null)

/**
 * Utilisation CPU réelle entre ce sondage et le précédent : les compteurs de
 * `/proc/stat` sont cumulatifs depuis le démarrage, seul un delta entre deux
 * sondages a un sens (`utilisation % = 100 × (1 − Δidle / Δtotal)`, bornée à
 * 0-100). `null` : pas encore de sondage précédent — le premier sondage
 * après l'arrivée sur la page ne peut pas afficher de pourcentage, ce n'est
 * pas une panne — ou `Δtotal <= 0` (deux sondages dans le même jiffy, ou des
 * compteurs revenus en arrière).
 */
function utilisationCpu(s: SystemPayload): number | null {
  const avant = precedentJiffies.value
  const total = s.cpu_total_jiffies
  const idle = s.cpu_idle_jiffies
  if (total != null && idle != null) precedentJiffies.value = { total, idle }
  if (total == null || idle == null || !avant) return null
  const deltaTotal = total - avant.total
  const deltaIdle = idle - avant.idle
  if (deltaTotal <= 0) return null
  return Math.min(100, Math.max(0, 100 * (1 - deltaIdle / deltaTotal)))
}

/**
 * Échantillon retenu dans l'historique, avec l'horodatage du sondage (pour
 * un futur survol, pas encore affiché). `null` si l'un des deux pourcentages
 * manque : une machine sans mémoire lisible, ou dont l'utilisation CPU n'est
 * pas encore calculable, garde un graphe vide plutôt qu'à moitié tracé. Une
 * conséquence à assumer : le premier échantillon exigeant lui-même un delta,
 * le graphe ne trace sa première ligne qu'au troisième sondage (deux pour
 * produire un échantillon, trois pour en avoir deux).
 */
function echantillon(s: SystemPayload, cpu: number | null): Echantillon | null {
  if (cpu == null || !s.memory || s.memory.total_kb === 0) return null
  return {
    cpu,
    ram: ((s.memory.total_kb - s.memory.available_kb) / s.memory.total_kb) * 100,
    temp: s.temperature_c ?? null,
    t: Date.now(),
  }
}

/**
 * Sondage, là où le reste de la SPA reçoit du SSE, et c'est délibéré : le
 * flux `/api/player` publie un état que le cœur produit de toute façon,
 * alors que ces métriques n'existent que parce qu'on les demande. Les
 * pousser ferait travailler en permanence un appareil le plus souvent
 * inactif, pour personne.
 *
 * Le sondage démarre au chargement de la SPA et vit jusqu'à la fermeture de la
 * page : ni le passage en arrière-plan ni le démontage de la vue ne l'arrêtent,
 * seule une action d'alimentation le suspend. C'est un renversement délibéré de
 * la note d'origine (« ne pas faire travailler un appareil le plus souvent
 * inactif ») : un graphe d'historique qui ne mesure que pendant qu'on le regarde
 * n'apprend rien, et une lecture de `/proc` toutes les 5 s ne coûte rien de
 * mesurable. L'IHM, en pratique, est rarement ouverte.
 *
 * Un échec n'affiche pas de toast : répété toutes les 5 secondes, un cœur
 * injoignable en produirait un flot. Une ligne de diagnostic suffit, comme
 * le drapeau `audioIndisponible` de la page de configuration.
 */
async function sonder() {
  // Verrou d'entrée : un sondage déjà en vol (minuteur qui tique plus vite
  // que la réponse n'arrive) n'en déclenche pas un second par-dessus, voir
  // le commentaire sur `sondageEnVol`.
  if (sondageEnVol) return
  // Après le verrou, pas avant : un appel repoussé par le verrou n'a rien
  // sondé, il ne doit donc pas repousser l'échéance.
  dernierSondage = Date.now()
  const controleur = new AbortController()
  sondageEnVol = controleur
  try {
    const s = await api.get<SystemPayload>('/api/system', { signal: controleur.signal })
    etat.value = s
    indisponible.value = false
    // Réarmement du loquet : la prochaine panne aura droit à sa ligne.
    echecSignale = false
    const cpu = utilisationCpu(s)
    utilisationCpuActuelle.value = cpu
    const p = echantillon(s, cpu)
    if (p) {
      historique.value.push(p)
      if (historique.value.length > CAPACITE) historique.value.shift()
    }
  } catch (e) {
    // Une annulation par `arreter()` (changement de période, suspension pour
    // une action d'alimentation) rejette aussi le `fetch` : ce n'est pas un
    // échec du cœur, juste notre propre requête coupée court, donc pas de
    // ligne « indisponible » pour ça.
    if (controleur.signal.aborted) return
    indisponible.value = true
    // Une seule ligne par panne, pas une par sondage : voir `echecSignale`.
    if (!echecSignale) {
      echecSignale = true
      console.warn('GET /api/system unavailable, staying quiet until it answers again', e)
    }
  } finally {
    if (sondageEnVol === controleur) sondageEnVol = null
  }
}

function demarrer() {
  // `suspendu` : une action d'alimentation en cours a déjà arrêté le sondage
  // normal (voir `confirmer`) ; le laisser reprendre ici — par ex. sur un
  // changement de période pendant un arrêt ou un redémarrage du service —
  // afficherait une erreur réseau alarmante sur un arrêt qui se déroule comme
  // demandé, ou sonderait en double avec `attendreRetour`.
  //
  // Plus de `document.hidden` ici : le sondage continue en arrière-plan, c'est
  // la raison d'être de ce store. Réserve mesurée et assumée — les navigateurs
  // brident les minuteurs d'un onglet caché (au moins 1 s, et environ un tic
  // par minute au-delà de quelques minutes), donc les échantillons pris pendant
  // une absence sont espacés, pas réguliers. L'axe des abscisses étant tiré des
  // horodatages (`abscisses`, dans `views/sparkline.ts`), le tracé reste juste ;
  // et le delta CPU aussi, les jiffies de `/proc/stat` étant cumulatifs — un
  // trou d'une minute donne une moyenne sur la minute, pas un chiffre faux.
  if (suspendu || minuteur !== null) return
  if (attente !== null) return
  // Reprise à l'échéance, pas sur-le-champ : changer la période ne doit pas
  // valoir un sondage. On ne sonde tout de suite que si le nouveau rythme rend
  // le précédent sondage déjà périmé — passer de 30 s à 1 s deux secondes après
  // le dernier, par exemple. Sinon on attend le temps qui restait à courir,
  // puis le rythme régulier reprend.
  //
  // La règle vaut aussi pour la reprise après une action d'alimentation, et
  // c'est voulu : une suspension plus courte que la période laisse à l'écran
  // des chiffres que la page elle-même juge encore frais, alors qu'une
  // interruption plus longue déclenche bien un sondage immédiat.
  const restant =
    dernierSondage === null ? 0 : Math.max(0, dernierSondage + periodeMs.value - Date.now())
  if (restant === 0) {
    void sonder()
    minuteur = setInterval(sonder, periodeMs.value)
    return
  }
  attente = setTimeout(() => {
    attente = null
    void sonder()
    minuteur = setInterval(sonder, periodeMs.value)
  }, restant)
}

function arreter() {
  if (minuteur !== null) {
    clearInterval(minuteur)
    minuteur = null
  }
  if (attente !== null) {
    clearTimeout(attente)
    attente = null
  }
  // Annule un sondage encore en vol : sans ça, un changement de période
  // laisserait une réponse plus ancienne atterrir après celle du nouveau
  // rythme et écraser `etat`/`precedentJiffies` avec des données périmées.
  if (sondageEnVol) {
    sondageEnVol.abort()
    sondageEnVol = null
  }
}

/**
 * Sondage suspendu par une action d'alimentation en cours.
 *
 * Remplace le test `enCours !== null` que faisait `demarrer()` quand tout
 * vivait dans la vue : le sondage est désormais partagé par toute la SPA et ne
 * peut plus lire l'état d'une page. La garde reste unique et reste ici — c'est
 * elle qui empêche un changement de période de rallumer le sondage sur un cœur
 * qu'on vient d'éteindre.
 *
 * Contrepartie à ne pas perdre de vue : laissé à `true`, il fige le graphe pour
 * *toutes* les pages, pas seulement celle qui a suspendu, et rien à l'écran ne
 * l'explique — `indisponible` reste faux, le graphe garde ses derniers points.
 * Tout chemin de sortie d'une action d'alimentation doit donc appeler
 * `reprendre()`. Une seule exception : l'**arrêt** de la machine, où l'appareil
 * s'en va pour de bon. Le redémarrage de la machine n'en est pas une, contre
 * l'intuition : le Pi revient en 20 à 40 s et l'onglet, lui, n'a pas bougé —
 * `confirmer` l'attend donc comme il attend le retour du service.
 */
let suspendu = false

/**
 * Sans `export`, délibérément, comme `arreter()` : la seule porte est l'objet
 * rendu par `useMetriques()`. Un `import { suspendre }` permettrait à
 * n'importe quel module de figer le sondage de toute la SPA sans passer par la
 * vue qui en répond — exactement ce que la privauté d'`arreter()` existe pour
 * empêcher, et le risque n'est pas théorique : un `suspendu` laissé à `true`
 * n'a pas d'autre remède qu'un rechargement complet de la page.
 */
function suspendre(): void {
  suspendu = true
  arreter()
}

function reprendre(): void {
  suspendu = false
  demarrer()
}

/**
 * Valeur de vue (chaîne, secondes) pour le sélecteur de période. Le
 * changement redémarre le sondage en repassant par `demarrer()` — sans le
 * contourner : c'est lui qui refuse de repartir pendant une action
 * d'alimentation en cours, et cette garde doit rester unique.
 */
const periode = computed({
  get: () => String(periodeMs.value / 1000),
  set: (v: string) => {
    const ms = Number(v) * 1000
    // Choisir à nouveau la période déjà active ne doit rien redéclencher :
    // sans ce garde-fou, chaque sélection — même sans changement — arrêtait
    // et relançait le sondage, avec un sondage immédiat superflu et une
    // fenêtre de delta CPU réinitialisée pour rien.
    if (ms === periodeMs.value) return
    periodeMs.value = ms
    arreter()
    demarrer()
  },
})

/**
 * Fenêtre visible de l'historique, en minutes : la durée réelle couverte par
 * `historique`, mesurée par l'horodatage de son premier et de son dernier
 * échantillon, et non la capacité théorique (`CAPACITE` × période) qui
 * suppose un tampon déjà plein. Cette hypothèse est fausse à l'arrivée sur la
 * page (tampon vide) et pendant les `CAPACITE` sondages qui suivent tout
 * changement de période : passer de 30 s à 1 s avec un tampon plein
 * afficherait sinon « 4 min » alors que le graphe trace encore 120 min
 * d'échantillons espacés de 30 s, et resterait faux pendant les `CAPACITE` sondages
 * suivants. Repli sur la capacité théorique seulement tant qu'il n'y a rien
 * à mesurer (moins de deux échantillons).
 */
const dureeFenetreMin = computed(() => {
  const h = historique.value
  if (h.length >= 2) return Math.round((h.at(-1)!.t - h[0]!.t) / 60000)
  return Math.round((CAPACITE * (periodeMs.value / 1000)) / 60)
})

/**
 * Remise à zéro complète. **Pour les tests uniquement** : l'état vit au niveau
 * module, donc sans ça un test laisse son historique, sa période et son
 * minuteur au suivant. À appeler dans un `beforeEach`.
 */
export function reinitialiserMetriques(): void {
  arreter()
  suspendu = false
  echecSignale = false
  dernierSondage = null
  etat.value = null
  indisponible.value = false
  historique.value = []
  periodeMs.value = 5000
  precedentJiffies.value = null
  utilisationCpuActuelle.value = null
}

export function useMetriques() {
  return {
    etat,
    indisponible,
    historique,
    utilisationCpuActuelle,
    periodeMs,
    periode,
    dureeFenetreMin,
    demarrer,
    suspendre,
    reprendre,
  }
}
