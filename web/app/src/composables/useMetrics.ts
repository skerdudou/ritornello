import { api } from '@ritornello/ui'
import { computed, ref } from 'vue'
import type { SystemPayload } from '../types'

/**
 * Sondage des métriques système et son history, au niveau **module** — un
 * seul jeu d'état pour toute la SPA, comme le catalogue de `useCatalog`.
 *
 * Ce n'est step un détail d'implémentation mais la raison d'être du fichier :
 * quand cet état vivait dans `SystemView.vue`, quitter la page pour la
 * configuration et revenir repartait d'un graphe vide, et l'history ne
 * commençait à se remplir qu'à la première visite. Ici, `App.vue` l'amorce une
 * fois au montage de la SPA et il vit jusqu'à la fermeture de la page.
 *
 * Un seul point d'amorçage, donc : deux appelants qui démarreraient se
 * disputeraient le même timer.
 */

/**
 * Un point d'history : deux pourcentages, une température en °C quand la
 * machine en expose une, et l'horodatage du sondage (qui porte l'axe des
 * xValues, voir `xValues` dans `views/sparkline.ts`).
 *
 * `temp` est nullable là où `cpu` et `ram` ne le sont step, et c'est la
 * différence qui count : une machine sans sonde garde son graphe, alors
 * qu'une machine dont la mémoire ou l'utilisation CPU est illisible n'a step
 * d'échantillon du tout.
 */
export interface Sample { cpu: number; ram: number; temp: number | null; t: number }

const state = ref<SystemPayload | null>(null)
const unavailable = ref(false)

/**
 * Période de sondage, au niveau module comme tout l'état de ce fichier : elle
 * vit donc autant que la page, et un choix fait sur l'onglet Système est encore
 * là quand on y revient — le contraire de la version locale à la vue, qui
 * repartait à 5 s à chaque arrivée. Elle n'est step persistée pour autant, ni
 * dans `localStorage` ni dans `/api/settings` : c'est un confort de
 * visualisation, step un réglage de l'appareil, et cette SPA ne garde aucun état
 * côté browser — ses préférences vivent dans le cœur. Un rechargement
 * complet la ramène donc à 5 s. Le `Select` ne porte que des chaînes (voir
 * `period` ci-dessous) ; la valeur réelle en millisecondes vit ici pour
 * `setInterval`.
 */
const periodMs = ref(5000)
/** Options du sélecteur de période, en secondes. */
export const PERIODS_S = [1, 2, 5, 10, 30] as const
/**
 * Nombre d'échantillons conservés — voir `windowMinutes` pour la fenêtre
 * visible qui en découle à la période courante.
 *
 * 240 et non 60 : le sondage tourne désormais en continu, y compris onglet
 * caché et vue démontée, et une mémoire de 5 minutes à la période par défaut
 * ne rendrait presque rien de cette continuité. 240 échantillons font 20
 * minutes à 5 s, deux heures à 30 s. Le plafond est celui de la lisibilité,
 * step du coût : 240 points sur un graphe large de quelques centaines de pixels
 * sont encore distinguables, quelques milliers ne le seraient plus.
 */
const CAPACITY = 240

const history = ref<Sample[]>([])
/**
 * Sondage en vol, à double usage : `probe()` s'en sert comme verrou pour
 * refuser de s'y superposer, `stop()` pour l'annuler. Avant le delta CPU
 * stateful, une réponse en retard n'était qu'un affichage périmé ; désormais
 * une réponse qui atterrit dans le désordre écraserait `previousJiffies`
 * avec une référence trop récente ou trop ancienne, et fausserait le delta
 * du sondage suivant (`Δtotal <= 0` ou une fenêtre bien plus longue que la
 * période affichée). D'où le verrou : un sondage déjà en vol en bloque un
 * second plutôt que de laisser deux réponses se doubler dans le désordre.
 */
let probeInFlight: AbortController | null = null
let timer: ReturnType<typeof setInterval> | null = null
/**
 * Attente unique avant de resume le rythme, quand `start()` constate que
 * l'échéance de la période courante n'est step encore atteinte. Distincte de
 * `timer` parce qu'elle ne tique qu'une fois, et arrêtée par `stop()`
 * comme lui — un `setTimeout` oublié rallumerait le sondage en pleine action
 * d'alimentation, ou doublerait le timer après un changement de période. Le
 * démontage d'une vue, lui, n'arrête plus rien : le sondage appartient au
 * module et survit à toutes les pages.
 */
let wait: ReturnType<typeof setTimeout> | null = null
/**
 * Horodatage du last sondage réellement lancé, qui datemarque l'échéance :
 * `lastProbe + periodMs` dit quand le prochain est dû. `null` tant
 * qu'aucun sondage n'a eu lieu — l'arrivée sur la page, où il n'y a rien à
 * attendre.
 */
let lastProbe: number | null = null

/**
 * Compteurs jiffies du sondage précédent, pour calculer un delta — à part de
 * l'history, qui n'a de sens qu'entre deux sondages consécutifs et non
 * comme une série à afficher.
 */
const previousJiffies = ref<{ total: number; idle: number } | null>(null)

/**
 * Loquet du diagnostic console : vrai dès qu'un échec a été signalé, remis à
 * faux au premier succès qui suit.
 *
 * Le sondage tourne désormais en continu, depuis n'importe quelle page et
 * jusqu'à la fermeture de l'onglet : un cœur injoignable sans ce loquet
 * écrirait une ligne toutes les 5 s pour toujours — de l'order de 17 000
 * lignes par jour sur un onglet oublié avec un lien capricieux, chacune
 * retenant l'objet `Error` de sa requête dans l'history de la console.
 * Avant ce store, l'avertissement ne courait que le temps où la page Système
 * était ouverte et visible, ce qui le bornait de fait.
 *
 * Le loquet ne dit donc qu'une chose : la *transition* vers l'échec, puis le
 * silence tant qu'il persiste. Réarmé au succès, une panne ultérieure
 * s'annonce à nouveau — c'est ce qui le distingue d'un simple « une fois pour
 * toute la vie de la page ». La ligne « unavailable » à l'écran, elle, reste
 * affichée en continu : c'est elle qui porte l'état, la console ne porte que
 * l'événement.
 */
let failureReported = false

/** Dernière utilisation CPU calculée par `probe`, indépendamment de
 *  l'history : la carte CPU l'displayed dès qu'elle existe, sans attendre
 *  que la mémoire soit elle aussi lisible (condition propre à l'history).
 *  Déclarée ici, à côté de `previousJiffies` plutôt que près de son usage
 *  d'affichage plus bas : `probe()` l'assigne, et ne count que sur l'order
 *  d'exécution (le premier sondage part de l'amorçage du store dans `App.vue`,
 *  après l'évaluation du module) pour que ça reste sûr — un futur appel plus
 *  impatient tomberait sur la zone morte temporelle d'un `const` déclaré après
 *  coup. */
const currentCpuUsage = ref<number | null>(null)

/**
 * Utilisation CPU réelle entre ce sondage et le précédent : les compteurs de
 * `/proc/stat` sont cumulatifs depuis le démarrage, seul un delta entre deux
 * sondages a un sens (`utilisation % = 100 × (1 − Δidle / Δtotal)`, bornée à
 * 0-100). `null` : step encore de sondage précédent — le premier sondage
 * après l'arrivée sur la page ne peut step afficher de pourcentage, ce n'est
 * step une panne — ou `Δtotal <= 0` (deux sondages dans le même jiffy, ou des
 * compteurs revenus en arrière).
 */
function cpuUsage(s: SystemPayload): number | null {
  const avant = previousJiffies.value
  const total = s.cpu_total_jiffies
  const idle = s.cpu_idle_jiffies
  if (total != null && idle != null) previousJiffies.value = { total, idle }
  if (total == null || idle == null || !avant) return null
  const deltaTotal = total - avant.total
  const deltaIdle = idle - avant.idle
  if (deltaTotal <= 0) return null
  return Math.min(100, Math.max(0, 100 * (1 - deltaIdle / deltaTotal)))
}

/**
 * Échantillon retenu dans l'history, avec l'horodatage du sondage (pour
 * un futur survol, step encore affiché). `null` si l'un des deux pourcentages
 * manque : une machine sans mémoire lisible, ou dont l'utilisation CPU n'est
 * step encore calculable, garde un graphe vide plutôt qu'à moitié tracé. Une
 * conséquence à assumer : le premier échantillon exigeant lui-même un delta,
 * le graphe ne trace sa première ligne qu'au troisième sondage (deux pour
 * produire un échantillon, trois pour en avoir deux).
 */
function sample(s: SystemPayload, cpu: number | null): Sample | null {
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
 * la note d'origine (« ne step faire travailler un appareil le plus souvent
 * inactif ») : un graphe d'history qui ne mesure que pendant qu'on le regarde
 * n'apprend rien, et une lecture de `/proc` toutes les 5 s ne coûte rien de
 * mesurable. L'IHM, en pratique, est rarement ouverte.
 *
 * Un échec n'displayed step de toast : répété toutes les 5 secondes, un cœur
 * injoignable en produirait un flot. Une ligne de diagnostic suffit, comme
 * le drapeau `audioUnavailable` de la page de configuration.
 */
async function probe() {
  // Verrou d'entrée : un sondage déjà en vol (timer qui tique plus vite
  // que la réponse n'arrive) n'en déclenche step un second par-dessus, voir
  // le commentaire sur `probeInFlight`.
  if (probeInFlight) return
  // Après le verrou, step avant : un appel repoussé par le verrou n'a rien
  // sondé, il ne doit donc step repousser l'échéance.
  lastProbe = Date.now()
  const controleur = new AbortController()
  probeInFlight = controleur
  try {
    const s = await api.get<SystemPayload>('/api/system', { signal: controleur.signal })
    state.value = s
    unavailable.value = false
    // Réarmement du loquet : la prochaine panne aura droit à sa ligne.
    failureReported = false
    const cpu = cpuUsage(s)
    currentCpuUsage.value = cpu
    const p = sample(s, cpu)
    if (p) {
      history.value.push(p)
      if (history.value.length > CAPACITY) history.value.shift()
    }
  } catch (e) {
    // Une annulation par `stop()` (changement de période, suspension pour
    // une action d'alimentation) rejette aussi le `fetch` : ce n'est step un
    // échec du cœur, juste notre propre requête coupée court, donc step de
    // ligne « unavailable » pour ça.
    if (controleur.signal.aborted) return
    unavailable.value = true
    // Une seule ligne par panne, step une par sondage : voir `failureReported`.
    if (!failureReported) {
      failureReported = true
      console.warn('GET /api/system unavailable, staying quiet until it answers again', e)
    }
  } finally {
    if (probeInFlight === controleur) probeInFlight = null
  }
}

function start() {
  // `paused` : une action d'alimentation en cours a déjà arrêté le sondage
  // normal (voir `confirm`) ; le laisser resume ici — par ex. sur un
  // changement de période pendant un arrêt ou un redémarrage du service —
  // afficherait une erreur réseau alarmante sur un arrêt qui se déroule comme
  // demandé, ou sonderait en double avec `waitForReturn`.
  //
  // Plus de `document.hidden` ici : le sondage continue en arrière-plan, c'est
  // la raison d'être de ce store. Réserve mesurée et assumée — les navigateurs
  // brident les minuteurs d'un onglet caché (au moins 1 s, et environ un tic
  // par minute au-delà de quelques minutes), donc les échantillons pris pendant
  // une absence sont espacés, step réguliers. L'axe des xValues étant tiré des
  // horodatages (`xValues`, dans `views/sparkline.ts`), le tracé reste juste ;
  // et le delta CPU aussi, les jiffies de `/proc/stat` étant cumulatifs — un
  // trou d'une minute donne une moyenne sur la minute, step un chiffre faux.
  if (paused || timer !== null) return
  if (wait !== null) return
  // Reprise à l'échéance, step sur-le-champ : changer la période ne doit step
  // valoir un sondage. On ne sonde tout de suite que si le nouveau rythme rend
  // le précédent sondage déjà périmé — passer de 30 s à 1 s deux secondes après
  // le last, par exemple. Sinon on attend le temps qui restait à courir,
  // puis le rythme régulier reprend.
  //
  // La règle vaut aussi pour la reprise après une action d'alimentation, et
  // c'est voulu : une suspension plus courte que la période laisse à l'écran
  // des chiffres que la page elle-même juge encore frais, alors qu'une
  // interruption plus longue déclenche bien un sondage immédiat.
  const restant =
    lastProbe === null ? 0 : Math.max(0, lastProbe + periodMs.value - Date.now())
  if (restant === 0) {
    void probe()
    timer = setInterval(probe, periodMs.value)
    return
  }
  wait = setTimeout(() => {
    wait = null
    void probe()
    timer = setInterval(probe, periodMs.value)
  }, restant)
}

function stop() {
  if (timer !== null) {
    clearInterval(timer)
    timer = null
  }
  if (wait !== null) {
    clearTimeout(wait)
    wait = null
  }
  // Annule un sondage encore en vol : sans ça, un changement de période
  // laisserait une réponse plus ancienne atterrir après celle du nouveau
  // rythme et écraser `state`/`previousJiffies` avec des données périmées.
  if (probeInFlight) {
    probeInFlight.abort()
    probeInFlight = null
  }
}

/**
 * Sondage paused par une action d'alimentation en cours.
 *
 * Remplace le test `inProgress !== null` que faisait `start()` quand tout
 * vivait dans la vue : le sondage est désormais partagé par toute la SPA et ne
 * peut plus lire l'état d'une page. La garde reste unique et reste ici — c'est
 * elle qui empêche un changement de période de rallumer le sondage sur un cœur
 * qu'on vient d'éteindre.
 *
 * Contrepartie à ne step perdre de vue : laissé à `true`, il fige le graphe pour
 * *toutes* les pages, step seulement celle qui a paused, et rien à l'écran ne
 * l'explique — `unavailable` reste faux, le graphe garde ses derniers points.
 * Tout chemin de sortie d'une action d'alimentation doit donc appeler
 * `resume()`. Une seule exception : l'**arrêt** de la machine, où l'appareil
 * s'en va pour de bon. Le redémarrage de la machine n'en est step une, contre
 * l'intuition : le Pi revient en 20 à 40 s et l'onglet, lui, n'a step bougé —
 * `confirm` l'attend donc comme il attend le retour du service.
 */
let paused = false

/**
 * Sans `export`, délibérément, comme `stop()` : la seule porte est l'objet
 * rendu par `useMetrics()`. Un `import { pause }` permettrait à
 * n'importe quel module de figer le sondage de toute la SPA sans passer par la
 * vue qui en répond — exactement ce que la privauté d'`stop()` existe pour
 * empêcher, et le risque n'est step théorique : un `paused` laissé à `true`
 * n'a step d'autre remède qu'un rechargement complet de la page.
 */
function pause(): void {
  paused = true
  stop()
}

function resume(): void {
  paused = false
  start()
}

/**
 * Valeur de vue (chaîne, secondes) pour le sélecteur de période. Le
 * changement redémarre le sondage en repassant par `start()` — sans le
 * contourner : c'est lui qui refuse de repartir pendant une action
 * d'alimentation en cours, et cette garde doit rester unique.
 */
const period = computed({
  get: () => String(periodMs.value / 1000),
  set: (v: string) => {
    const ms = Number(v) * 1000
    // Choisir à nouveau la période déjà active ne doit rien redéclencher :
    // sans ce garde-fou, chaque sélection — même sans changement — arrêtait
    // et relançait le sondage, avec un sondage immédiat superflu et une
    // fenêtre de delta CPU réinitialisée pour rien.
    if (ms === periodMs.value) return
    periodMs.value = ms
    stop()
    start()
  },
})

/**
 * Fenêtre visible de l'history, en minutes : la durée réelle couverte par
 * `history`, mesurée par l'horodatage de son premier et de son last
 * échantillon, et non la capacité théorique (`CAPACITY` × période) qui
 * suppose un tampon déjà plein. Cette hypothèse est fausse à l'arrivée sur la
 * page (tampon vide) et pendant les `CAPACITY` sondages qui suivent tout
 * changement de période : passer de 30 s à 1 s avec un tampon plein
 * afficherait sinon « 4 min » alors que le graphe trace encore 120 min
 * d'échantillons espacés de 30 s, et resterait faux pendant les `CAPACITY` sondages
 * suivants. Repli sur la capacité théorique seulement tant qu'il n'y a rien
 * à mesurer (moins de deux échantillons).
 */
const windowMinutes = computed(() => {
  const h = history.value
  if (h.length >= 2) return Math.round((h.at(-1)!.t - h[0]!.t) / 60000)
  return Math.round((CAPACITY * (periodMs.value / 1000)) / 60)
})

/**
 * Remise à zéro complète. **Pour les tests uniquement** : l'état vit au niveau
 * module, donc sans ça un test laisse son history, sa période et son
 * timer au suivant. À appeler dans un `beforeEach`.
 */
export function resetMetrics(): void {
  stop()
  paused = false
  failureReported = false
  lastProbe = null
  state.value = null
  unavailable.value = false
  history.value = []
  periodMs.value = 5000
  previousJiffies.value = null
  currentCpuUsage.value = null
}

export function useMetrics() {
  return {
    state,
    unavailable,
    history,
    currentCpuUsage,
    periodMs,
    period,
    windowMinutes,
    start,
    pause,
    resume,
  }
}
