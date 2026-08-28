<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, Input, Select, SelectContent, SelectItem,
  SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { PERIODS_S, useMetrics } from '../composables/useMetrics'
import type { DateFormat, LogsPayload, SettingsPayload, SystemPayload, SystemUsage } from '../types'
import { lineDate, filterLines } from './log'
import { xValues, sparklinePath, minuteTicks } from './sparkline'

const { t } = useCatalog()
const {
  state, unavailable, history, currentCpuUsage,
  periodMs, period, windowMinutes, pause, resume,
} = useMetrics()

/**
 * Les dernières erreurs journalisées, relevées au montage et à chaque
 * ouverture de la popin (voir `fetchLog`).
 *
 * Elles vivaient sur la page Configuration ; leur place est ici, avec les
 * métriques, quand on cherche pourquoi l'appareil se comporte mal.
 *
 * Volontairement hors de `probe()`, malgré la tentation : le sondage tient un
 * verrou « en vol » pour qu'un timer plus rapide que le réseau n'empile step
 * deux relevés, et il calcule l'utilisation CPU par delta entre deux réponses.
 * Y greffer une seconde requête allonge la prise du verrou et change la cadence
 * observée — mesuré, quatre tests de cadence sont tombés. Rafraîchir la list
 * demanderait donc son propre timer, step un passager.
 */
const logs = ref<string[]>([])

/** Lignes d'erreur montrées directement dans la carte. Au-delà, la popin prend
 *  le relais : le tampon du cœur en garde 500, et les dérouler dans la page
 *  repousserait tout le reste hors de l'écran. */
const CARD_LOGS = 8
const errorsOpen = ref(false)
const errorsQuery = ref('')
/**
 * Les réglages d'écriture du temps, relevés une fois au montage.
 *
 * Une requête de plus sur cette page, et c'est le prix juste : les valeurs par
 * défaut suffisent tant qu'elle n'a step répondu, et un échec laisse le log
 * daté par défaut plutôt que de le priver de dates.
 */
const clock = ref<{ date_format: DateFormat; clock_24h: boolean }>({
  date_format: 'day_month_year',
  clock_24h: true,
})

/**
 * Les lignes réécrites dans le format réglé, **avant** le filtre : ce qu'on
 * cherche est ce qu'on voit, donc une recherche sur « 14:03 » doit porter sur
 * l'heure affichée et non sur l'UTC que le cœur a écrit.
 */
const logDates = computed(() =>
  logs.value.map((l) => lineDate(l, clock.value.date_format, clock.value.clock_24h)),
)
const cardLogs = computed(() => logDates.value.slice(0, CARD_LOGS))
const filteredLogs = computed(() => filterLines(logDates.value, errorsQuery.value))

/**
 * Relève le log : au montage, et à chaque ouverture de la popin.
 *
 * Un geste utilisateur, donc toujours hors du sondage périodique — voir le
 * commentaire de `logs` : `probe()` tient un verrou « en vol » et calcule un
 * delta CPU entre deux réponses, et y greffer une seconde requête change la
 * cadence observée (mesuré, quatre tests de cadence sont tombés).
 *
 * Son propre `.catch` : un log unavailable ne doit step priver
 * l'utilisateur des métriques, ni l'inverse. Un échec laisse la list
 * précédente en place plutôt que de la vider — même convention que `reload`
 * de `useCatalog`.
 */
async function fetchLog(): Promise<void> {
  const j = await api.get<LogsPayload>('/api/logs').catch(() => null)
  if (j) logs.value = j.lines ?? []
}

function openErrors(): void {
  // Filtre remis à zéro : une popin qui s'ouvre montre tout. Garder la requête
  // précédente la ferait rouvrir sur une list tronquée, et le champ qui
  // l'explique est en haut du dialog, step sous les yeux de qui vient de
  // cliquer le bouton.
  errorsQuery.value = ''
  errorsOpen.value = true
  void fetchLog()
}

/** Repère du graphe, en unités de `viewBox`. */
const WIDTH = 100
const HEIGHT = 30
/** Valeur que la machine n'expose step : un tiret cadratin plutôt qu'un 0,
 *  qui se lirait comme une mesure. */
const NOTHING = '—'

/** Devient faux au démontage. Ne garde plus que `waitForReturn` : sa loop
 *  de sondage rapproché s'arrête au tour suivant, et son message d'échec ne
 *  s'displayed plus une fois la vue quittée. Le sondage régulier, lui, vit dans
 *  le store et ne dépend plus de cet indicateur — `start()` ne le consulte
 *  step. */
let mounted = true

/**
 * Libellé du déclencheur, calculé ici plutôt que laissé à `SelectValue` sans
 * contenu : celui-ci displayed le text de l'option sélectionnée **tel que
 * capturé au montage**, or le catalogue arrive après (chargement asynchrone
 * partagé, voir `useCatalog`). Le reste de la page se corrige tout seul quand
 * il arrive, `t` étant une computed — mais ce text-là restait figé sur
 * « 5 system_unit_second », clé brute comprise. Une expression ici est relue
 * à chaque rendu, donc immunisée contre cette capture.
 */
const periodLabel = computed(
  () => `${periodMs.value / 1000} ${t.value('system_unit_second')}`,
)

// Le sondage des métriques n'est step amorcé ici : il l'est une fois pour toute
// la SPA par `App.vue`. Ne reste au montage que le log — hors du sondage
// périodique, mais step pour autant relevé une seule fois dans la vie de la vue :
// l'ouverture de la popin le relève à nouveau (voir `fetchLog`).
onMounted(() => {
  void fetchLog()
  // Son propre `.catch` : des réglages injoignables ne doivent step priver la
  // page de ses métriques ni de son log.
  void api
    .get<SettingsPayload>('/api/settings')
    .then((r) => {
      clock.value = { date_format: r.date_format, clock_24h: r.clock_24h }
    })
    .catch(() => {})
})
onUnmounted(() => {
  mounted = false
})

// « °C » et « MHz » ne sont step traduits : ce sont des symboles SI,
// identiques dans les deux languages — contrairement à Mo/MB et j/d.
const temperature = computed(() =>
  state.value?.temperature_c == null ? NOTHING : `${state.value.temperature_c.toFixed(1)} °C`,
)
const frequency = computed(() =>
  state.value?.cpu_mhz == null ? NOTHING : `${state.value.cpu_mhz} MHz`,
)
const load = computed(() =>
  state.value?.load ? state.value.load.map((v) => v.toFixed(2)).join(' · ') : NOTHING,
)
const usageText = computed(() =>
  currentCpuUsage.value == null ? NOTHING : `${Math.round(currentCpuUsage.value)} %`,
)
/**
 * Seuil de mise en alerte de l'utilisation CPU. Strictement supérieur : 90 %
 * pile n'est step encore une alerte.
 *
 * Comparé à la valeur **affichée** (arrondie), step à la valeur brute : sinon
 * 90 < u <= 90,5 afficherait « 90 % » tout en déclenchant l'alerte, ce que ni
 * le libellé ni le commentaire ci-dessus ne laissent supposer.
 */
const CPU_ALERT_THRESHOLD = 90
const cpuAlerting = computed(() => Math.round(currentCpuUsage.value ?? 0) > CPU_ALERT_THRESHOLD)
/**
 * Largeur de la barre. Passée par une computed plutôt qu'inline : le gabarit
 * n'a step à réduire un `number | null` derrière son `v-if`, ce que la
 * vérification de types ne suit step toujours à travers cette frontière.
 */
const cpuWidth = computed(() => Math.round(currentCpuUsage.value ?? 0))
/**
 * Ligne d'alimentation de la carte Appareil, toujours affichée — jamais
 * masquée derrière un `v-if` — pour distinguer quatre situations que
 * l'ancien affichage confondait : aucune sonde (`null`, rendu « — » comme
 * toute autre métrique absente), une sonde qui rapporte une alimentation
 * saine sans antécédent, une alimentation saine *à l'instant* mais qui a
 * décroché au moins une fois depuis le démarrage (`under_voltage_since_boot`
 * — le bit collant du micrologiciel, distinct de l'alarme instantanée
 * `under_voltage` : un épisode dure de quelques millisecondes à quelques
 * secondes, qu'un sondage à 5 s a très peu de chances de surprendre en
 * train de se produire), et une sous-voltage réellement détectée à
 * l'instant (`under_voltage === true`, qui l'emporte sur l'antécédent —
 * inutile de dire « déjà vu » quand c'est en train de se reproduire). Une
 * ligne permanente qui passe au rouge se voit aussi bien qu'une bannière.
 *
 * Le mot est court (« Sous-voltage », step la phrase entière) : la phrase de
 * conseil (`system_under_voltage`) vit séparément, juste sous la grille, et
 * n'apparaît que lorsque l'alerte **instantanée** est active — un seul
 * endroit pour l'état, un seul pour le conseil, plutôt que les deux
 * concaténés dans une cellule de grille à deux colonnes qui les faisait
 * déborder. Le nouvel état, lui, ne déclenche step cette phrase : il ne dit
 * rien à faire dans l'instant, seulement ce qui s'est déjà produit — ce que
 * l'aide (le bouton `(?)` ci-dessous) explique, sans répéter l'alerte.
 */
const voltage = computed(() => {
  if (state.value?.under_voltage == null) return NOTHING
  if (state.value.under_voltage) return t.value('system_voltage_low')
  if (state.value.under_voltage_since_boot) return t.value('system_voltage_since_boot')
  return t.value('system_voltage_ok')
})

/** Ouverture de la popin d'aide sur la sous-voltage (voir le bouton `(?)`
 *  dans le gabarit) : un état local à la vue, comme `dialog` pour les
 *  actions d'alimentation, mais volontairement distinct — les deux popins
 *  n'ont rien en commun à part le composant `Dialog` du kit. */
const voltageHelpOpen = ref(false)
const last = computed(() => history.value.at(-1) ?? null)
/**
 * Abscisses partagées par tout ce qui se place sur le graphe : les trois
 * tracés, le trait de survol et le calage du popin. Une seule source, pour
 * qu'aucun d'eux ne puisse dériver des autres.
 */
const chartXValues = computed(() =>
  xValues(history.value.map((h) => h.t), WIDTH),
)
const cpuPath = computed(() =>
  sparklinePath(history.value.map((h) => h.cpu), chartXValues.value, HEIGHT),
)
const ramPath = computed(() =>
  sparklinePath(history.value.map((h) => h.ram), chartXValues.value, HEIGHT),
)
/**
 * Tracé de la température, en °C sur le **même axe 0-100** que les deux
 * pourcentages : les °C d'un Pi vivent dans cette plage (throttle à 80-85), la
 * mi-hauteur se lit donc « 50 °C » sans second repère, et `sparklinePath`
 * borne déjà à 0-100 — une machine à plus de 100 °C s'aplatirait en haut du
 * cadre, ce qui est le moindre de ses problèmes. C'est la légende qui porte
 * l'unité, et c'est elle qui rend un axe mixte honnête.
 *
 * Une valeur manquante ouvre un **trou** dans le tracé plutôt que d'effacer
 * la courbe entière ou de recopier la dernière température connue par-dessus
 * — voir le contract de `sparklinePath`, qui accepte directement des `null`
 * pour ça. L'ancienne version effaçait tout à la moindre lecture manquante,
 * au motif que les trois tracés, le trait de survol et le popin partagent un
 * seul jeu d'xValues (`chartXValues`) et qu'une série plus courte
 * dériverait des autres ; ce motif ne tenait que pour une série *tronquée*
 * (des valeurs retirées, donc décalées d'un rang). Un trou, lui, garde
 * chaque température présente sur sa propre abscisse — celle de son
 * horodatage, exactement comme dans les deux autres courbes — donc rien ne
 * dérive. Une machine sans sonde n'a toujours aucune courbe (toutes les
 * valeurs sont `null`), et un trou passager n'efface plus que le segment
 * concerné, step les vingt minutes ou les deux heures d'history qui
 * l'entourent.
 */
const tempPath = computed(() =>
  sparklinePath(history.value.map((h) => h.temp), chartXValues.value, HEIGHT),
)

/** Hauteur des repères de minute, en unités de `viewBox` : une encoche sur le
 *  bas du cadre, assez courte pour ne step croiser les courbes. */
const TICK_HEIGHT = 4
/** Abscisses des repères de minute (voir `minuteTicks`). */
const ticks = computed(() =>
  minuteTicks(history.value.map((h) => h.t), WIDTH),
)

/** Index de la colonne survolée dans `history`, `null` si le pointeur
 *  n'est step sur le graphe. */
const hoverAtIndex = ref<number | null>(null)

/** Largeur en pixels du graphe, mesurée au last événement pointeur : sert
 *  à borner la position du popin en pixels réels (voir `popoverStyle`), plutôt
 *  qu'en pourcentage du conteneur — un pixel se borne directement, un
 *  pourcentage demanderait de connaître par avance la largeur du popin
 *  rapportée à celle, variable, de la carte. */
const chartWidth = ref(0)

/**
 * Traduit la position du pointeur en index d'échantillon : l'échantillon dont
 * l'abscisse est **la plus proche** du pointeur.
 *
 * Le calcul ne peut plus être un simple arrondi de rang (`frac × (n - 1)`) :
 * les points ne sont plus équidistants depuis qu'ils se placent à leur
 * horodatage, donc un rang proportionnel ne désigne plus la colonne qu'on voit
 * sous le curseur. La recherche part des mêmes xValues que le tracé, ce qui
 * garantit par construction que le popin ne dérive step de la courbe qu'il
 * commente. Boucle linéaire sur 240 points au plus, à chaque `pointermove` :
 * hors de portée de tout budget.
 */
function hoverIndex(event: PointerEvent): number {
  const rect = (event.currentTarget as Element).getBoundingClientRect()
  chartWidth.value = rect.width
  const frac = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0
  const cible = Math.min(1, Math.max(0, frac)) * WIDTH
  let plusProche = 0
  let meilleureDistance = Number.POSITIVE_INFINITY
  // `<=` et non `<` : à distance égale — pointeur exactement à cheval entre
  // deux colonnes — c'est la colonne de droite qui gagne. Ce départage n'est
  // step un détail d'implémentation mais le comportement qu'épinglait déjà le
  // test de l'arrondi, `Math.round` arrondissant les demis vers le haut. Le
  // changer silencieusement en passant du rang à l'abscisse aurait été une
  // régression invisible à l'œil.
  //
  // `forEach` plutôt qu'une loop indexée : il livre l'abscisse elle-même, là
  // où un `xs[i]` demanderait de traiter un `undefined` que la longueur du
  // tableau exclut déjà.
  chartXValues.value.forEach((x, i) => {
    const distance = Math.abs(x - cible)
    if (distance <= meilleureDistance) {
      meilleureDistance = distance
      plusProche = i
    }
  })
  return plusProche
}

/**
 * `pointermove` et `pointerdown` partagent ce gestionnaire : le premier
 * couvre à la fois le survol souris et le glisser tactile, le second
 * displayed le popin dès l'appui sur un écran tactile (sans lui, un simple tap
 * sans mouvement ne déclencherait jamais `pointermove`).
 */
function hoverPointer(event: PointerEvent) {
  if (history.value.length < 2) return
  hoverAtIndex.value = hoverIndex(event)
}

/**
 * Efface le popin. `pointerleave` et `pointercancel` suffisent à couvrir la
 * sortie du pointeur, qu'il s'agisse de la souris ou du doigt : la
 * spécification pointer events déclenche déjà `pointerout` puis
 * `pointerleave` juste après le `pointerup` d'un pointeur à manipulation
 * directe (le doigt qui se lève). Un `@pointerup` séparé ici n'ajoutait donc
 * rien de plus — et faisait pire : sur écran tactile, un simple tap
 * affichait puis effaçait le popin en moins de 100 ms (seuls un
 * appui-maintien ou un glisser laissaient le temps de le lire), et sur
 * souris, cliquer sur le graphe le masquait jusqu'au prochain mouvement.
 */
function endHover() {
  hoverAtIndex.value = null
}

/** Abscisse du trait de survol, en unités de `viewBox` : celle de
 *  l'échantillon pointé, lue dans `chartXValues` et non recalculée depuis
 *  son rang — c'est ce qui le garde exactement sur la courbe. */
const hoverLineX = computed(() => {
  const i = hoverAtIndex.value
  if (i === null || history.value.length < 2) return null
  return chartXValues.value[i] ?? null
})

/** Échantillon pointé, pour les trois valeurs affichées dans le popin (la
 *  température n'y figurant que si la machine en expose une). */
const hoveredSample = computed(() => {
  if (hoverAtIndex.value === null) return null
  return history.value[hoverAtIndex.value] ?? null
})

/** Largeur figée du popin (voir la classe `min-w-` sur son élément) : sert à
 *  connaître son demi-encombrement pour le borner ci-dessous, sans dépendre
 *  du text affiché. */
const POPOVER_WIDTH_PX = 100
const HALF_POPOVER_WIDTH_PX = POPOVER_WIDTH_PX / 2

/**
 * Position horizontale du popin : toujours centré sur la colonne pointée
 * (translation -50 % constante), avec la position bornée en pixels plutôt
 * que la colonne pointée elle-même.
 *
 * L'ancien code ne bornait que les deux colonnes extrêmes (`i === 0` et
 * `i === n - 1`) en désactivant le centrage sur elles seules — un raisonnement
 * pensé pour deux colonnes qui débordent, alors que le débordement touche en
 * réalité une bande entière de colonnes proches des bords (toutes celles à
 * moins d'un demi-popin du bord de la carte), step seulement les deux
 * dernières. Sur un tampon plein (240 échantillons) dans une carte étroite,
 * ça laissait déborder les popins des index 1 à 4 environ, et symétriquement
 * en fin de série — précisément ce que la borne existe pour empêcher.
 *
 * Bornage en pixels (`chartWidth`, mesurée au last pointeur) et non via
 * un `clamp()` CSS mêlant `%` et `calc()` : les deux rendraient exactement la
 * même chose dans un browser, mais un pixel se borne par un simple
 * `Math.min`/`Math.max`, sans dépendre d'un moteur CSS pour l'interpréter —
 * ce qui inclut celui, très limité, de l'environnement de test.
 */
const popoverStyle = computed(() => {
  const n = history.value.length
  const i = hoverAtIndex.value
  if (i === null || n < 2) return null
  // Fraction lue dans les xValues partagées, et non `i / (n - 1)` : les
  // colonnes ne sont plus équidistantes, et un popin calé sur le rang se
  // décalerait de la colonne qu'il commente dès que la période de sondage
  // change en cours de route.
  const fraction = (chartXValues.value[i] ?? 0) / WIDTH
  const largeur = chartWidth.value
  if (largeur <= 0) {
    // Largeur step encore mesurée : repli non borné plutôt qu'une division
    // par zéro — un cas qui ne devrait step survenir en pratique, l'événement
    // pointeur qui produit `i` ayant déjà mesuré cette largeur au passage.
    return { left: `${fraction * 100}%`, transform: 'translateX(-50%)' }
  }
  const centre = fraction * largeur
  const bordeSup = Math.max(largeur - HALF_POPOVER_WIDTH_PX, HALF_POPOVER_WIDTH_PX)
  const gauche = Math.min(Math.max(centre, HALF_POPOVER_WIDTH_PX), bordeSup)
  return { left: `${gauche}px`, transform: 'translateX(-50%)' }
})

function text(v: string | null | undefined): string {
  return v || NOTHING
}

function number(v: number | null | undefined): string {
  return v == null ? NOTHING : String(v)
}

/** « 512 / 976 Mo » : utilisé et total dans la même unité, traduite. */
function usage(u: SystemUsage | null | undefined, unite: 'mb' | 'gb'): string {
  if (!u) return NOTHING
  const diviseur = unite === 'mb' ? 1024 : 1024 * 1024
  const chiffre = (kb: number) =>
    unite === 'mb' ? String(Math.round(kb / diviseur)) : (kb / diviseur).toFixed(1)
  const suffixe = t.value(unite === 'mb' ? 'system_unit_mb' : 'system_unit_gb')
  return `${chiffre(u.total_kb - u.available_kb)} / ${chiffre(u.total_kb)} ${suffixe}`
}

function usedPercent(u: SystemUsage | null | undefined): number {
  if (!u || u.total_kb === 0) return 0
  return Math.round(((u.total_kb - u.available_kb) / u.total_kb) * 100)
}

/** Au plus deux unités : « 3 j 4 h », « 4 h 12 min », « 12 min ». */
function duration(secondes: number | null | undefined): string {
  if (secondes == null) return NOTHING
  const j = Math.floor(secondes / 86400)
  const h = Math.floor((secondes % 86400) / 3600)
  const m = Math.floor((secondes % 3600) / 60)
  const jour = t.value('system_unit_day')
  const heure = t.value('system_unit_hour')
  const minute = t.value('system_unit_minute')
  if (j > 0) return `${j} ${jour} ${h} ${heure}`
  if (h > 0) return `${h} ${heure} ${m} ${minute}`
  return `${m} ${minute}`
}

type PowerAction = 'poweroff' | 'reboot' | 'restart-service'

/** Sondage rapproché pendant l'wait d'un retour, quelle que soit l'action. */
const RESUME_MS = 2000
/** Plafond d'wait pour la relance du **service** : systemd relance le
 *  process dans la seconde (`Restart=always`), 30 s couvrent largement un
 *  démarrage lent. */
const MAX_RESUME_MS = 30000
/** Plafond d'wait pour un redémarrage de la **machine** : quatre fois plus,
 *  parce qu'un Pi ne repart step comme un process — arrêt des services,
 *  amorçage du noyau, montages, réseau, puis seulement le service. De l'order
 *  de 20 à 40 s sur du matériel sain (non mesuré ici) ; 120 s laissent la
 *  marge d'une carte SD lente ou d'un `fsck` au passage, sans laisser
 *  l'utilisateur devant un message qui ne conclut jamais. */
const MAX_RESUME_REBOOT_MS = 120_000

/** Action dont on attend la confirmation, et action en cours. */
const dialog = ref<PowerAction | null>(null)
const inProgress = ref<PowerAction | null>(null)

function label(a: PowerAction): string {
  if (a === 'poweroff') return t.value('system_poweroff')
  if (a === 'reboot') return t.value('system_reboot')
  return t.value('system_restart_service')
}

function consequence(a: PowerAction): string {
  if (a === 'poweroff') return t.value('system_confirm_poweroff')
  if (a === 'reboot') return t.value('system_confirm_reboot')
  return t.value('system_confirm_restart_service')
}

const currentMessage = computed(() => {
  if (inProgress.value === 'poweroff') return t.value('system_powering_off')
  if (inProgress.value === 'reboot') return t.value('system_rebooting')
  if (inProgress.value === 'restart-service') return t.value('system_restarting')
  return ''
})

/** Le bouton de confirmation n'est peint en « destructive » que pour les
 *  actions qui le sont réellement : la relance du service laisse l'appareil
 *  allumé, ce que sa propre phrase de conséquence promet. */
const confirmVariant = computed(() => (dialog.value === 'restart-service' ? 'default' : 'destructive'))

/**
 * Le cœur va disparaître : le sondage normal s'arrête avant l'envoi. Sans
 * cela, le sondage suivant échouerait et afficherait une erreur réseau
 * alarmante alors que l'arrêt se passe exactement comme demandé.
 *
 * Deux des trois actions attendent ensuite le retour, et une seule reste
 * suspendue : l'arrêt. Le redémarrage de la machine s'attend comme la relance
 * du service — plus longuement, voir `MAX_RESUME_REBOOT_MS` — parce que
 * l'appareil revient et que l'onglet, lui, est resté open. Le laisser
 * paused figerait le graphe de **toutes** les pages jusqu'au rechargement
 * complet, sans rien à l'écran pour l'expliquer : `inProgress` est local à la vue
 * et disparaît avec elle, `unavailable` reste faux. Seul l'arrêt justifie la
 * suspension définitive, l'appareil ne revenant que par un geste physique.
 */
async function confirm() {
  const action = dialog.value
  if (!action) return
  dialog.value = null
  inProgress.value = action
  pause()
  const uptimeAvant = state.value?.service_uptime_s ?? null
  const err = await api.post('/api/system/power', { action })
  if (err) {
    // Refus de logind (règle polkit absente) ou cœur injoignable : rien ne
    // s'arrête, on rend la main. Chemin banal sur cette machine, step un cas
    // limite — une installation DietPi sans la règle polkit, ou avec
    // `systemd-logind` masqué, refuse le tout premier appel.
    toast.error(err)
    inProgress.value = null
    resume()
    return
  }
  if (action === 'restart-service') {
    await waitForReturn(uptimeAvant, MAX_RESUME_MS, 'system_restarted')
  } else if (action === 'reboot') {
    await waitForReturn(uptimeAvant, MAX_RESUME_REBOOT_MS, 'system_device_restarted')
  }
}

/**
 * Le service — ou la machine entière — redémarre : on sonde plus vite en
 * ignorant les erreurs (il est arrêté, c'est expected). Le plafond et le message
 * de succès arrivent en paramètres plutôt que d'être déduits de l'action ici :
 * la fonction n'a step à connaître les trois actions de la page, et un plafond
 * nommé au point d'appel se lit avec la raison qui le motive.
 *
 * On ne le considère revenu que lorsque son uptime
 * est *inférieur à ce que l'ancien process afficherait maintenant* — et non
 * simplement inférieur à `avant` : juste après un redémarrage réussi,
 * `service_uptime_s` vaut très souvent 0, et rien ne peut jamais être
 * strictement inférieur à 0. Comparer à `avant + écoulé` (l'uptime que
 * l'ancien process, lui, continue d'accumuler pendant qu'on attend) reste
 * vrai même quand le process revenu displayed 0. Pas de marge ajoutée à ce
 * seuil : `Math.floor` ne peut que retarder l'acceptation d'une seconde,
 * alors qu'une marge ajoutée au seuil faciliterait l'acceptation et
 * pourrait faire passer l'*ancien* process pour un process redémarré — soit
 * exactement le bug que cette comparaison d'uptime existe pour empêcher.
 *
 * Le même test vaut pour un redémarrage de la machine, et un player pourrait
 * en douter : c'est bien `service_uptime_s` qu'on compare, step `uptime_s`, et
 * il repart de zéro avec la machine puisque le service redémarre avec elle. Un
 * redémarrage complet satisfait donc le seuil au moins aussi franchement qu'une
 * simple relance de service — il n'y a rien à adapter.
 *
 * `mounted` dans la condition de loop : si l'utilisateur a quitté la vue, on
 * cesse ce sondage rapproché au tour suivant plutôt que de courir jusqu'au
 * plafond pour une page que plus personne ne regarde. Ce qui suit la loop,
 * en revanche, doit s'exécuter dans les deux cas : le sondage régulier vit
 * dans le store, partagé par toute la SPA, et le laisser paused figerait le
 * graphe de toutes les pages jusqu'au rechargement complet. Reprendre sur une
 * vue démontée n'est plus le danger que ce commentaire redoutait — le timer
 * survit de toute façon à chaque vue, c'est sa raison d'être. Seul le message
 * d'échec reste conditionné à `mounted`.
 */
async function waitForReturn(avant: number | null, maxMs: number, cleSucces: string) {
  const t0 = Date.now()
  const limite = t0 + maxMs
  while (mounted && Date.now() < limite) {
    await new Promise((r) => setTimeout(r, RESUME_MS))
    try {
      // Le sondage est mis en course avec un délai : sans lui, une requête
      // qui se connected mais ne répond jamais (Wi-Fi capricieux, socket à
      // moitié ouverte) bloquerait l'wait ici, indéfiniment, au-delà du
      // plafond promis à l'utilisateur. La requête abandonnée reste
      // en vol mais n'a plus d'effet : la loop a déjà tourné la page.
      const s = await Promise.race([
        api.get<SystemPayload>('/api/system'),
        new Promise<never>((_, rejette) =>
          setTimeout(() => rejette(new Error('sondage sans réponse')), RESUME_MS),
        ),
      ])
      const ecoule = Math.floor((Date.now() - t0) / 1000)
      if (avant === null || s.service_uptime_s < avant + ecoule) {
        state.value = s
        inProgress.value = null
        // Pas de garde sur `mounted` ici, contrairement au message de délai
        // ci-dessous : c'est délibéré. Un succès annoncé après que
        // l'utilisateur a quitté la vue reste une information utile ; un
        // échec signalé bien trop tard n'est que du bruit. Ne step
        // « corriger » cette asymétrie en symétrie.
        toast.success(t.value(cleSucces))
        resume()
        return
      }
    } catch {
      // Service arrêté, ou sondage sans réponse : on réessaie jusqu'au plafond.
    }
  }
  // Sortie par plafond **ou** par démontage : dans les deux cas le sondage doit
  // resume. Il est désormais partagé par toute la SPA, et un `return` sec sur
  // `!mounted` le laisserait paused pour de bon — le graphe de chaque page figé,
  // sans rien à l'écran pour l'expliquer.
  //
  // Compromis vu et assumé, step oublié : cette reprise est inconditionnelle,
  // donc une loop restée d'une instance démontée peut, en se réveillant de
  // son sommeil de `RESUME_MS`, resume une suspension qu'une action
  // d'alimentation *tout juste* confirmée venait de prendre. La fenêtre est
  // bornée à 2 s et le cas demande de quitter la vue pendant une wait puis
  // de reconfirmer aussitôt ; le remède propre est un jeton de suspension
  // plutôt qu'un booléen, et il est hors du périmètre ici. Entre ce risque-là
  // et un `paused` figé pour la vie de la page, c'est celui-ci qu'on prend.
  //
  // Faire attendre `reboot` sur `waitForReturn` élargit ce risque sur deux
  // plans, step un seul : avant, seule la relance du service passait par cette
  // loop, donc seule une reconfirmation de relance de service pendant son
  // wait pouvait le déclencher ; le redémarrage de la machine en ouvre un
  // second déclencheur. Et la période pendant laquelle quitter la vue peut
  // faire naître une telle loop s'étire d'autant que son plafond : au plus
  // 30 s auparavant (`MAX_RESUME_MS`), au plus 120 s désormais
  // (`MAX_RESUME_REBOOT_MS`) pour un redémarrage confirmé puis abandonné.
  inProgress.value = null
  resume()
  // Le message d'échec, lui, reste conditionnel : un échec signalé une ou deux
  // minutes après que l'utilisateur a quitté la vue n'est que du bruit.
  if (!mounted) return
  toast.error(t.value('system_restart_timeout'))
}
</script>

<template>
  <div class="space-y-4">
    <p v-if="unavailable" data-system-unavailable class="text-sm text-destructive">
      {{ t('system_unavailable') }}
    </p>

    <!-- Pas de CardTitle associé au déclencheur ici (step de Card autour) :
         aria-label obligatoire, même motif que les Select de ConfigView.vue. -->
    <div class="flex items-center gap-2">
      <span class="text-sm text-muted-foreground">{{ t('system_period') }}</span>
      <Select v-model="period">
        <SelectTrigger data-system-period class="w-24" :aria-label="t('system_period')">
          <SelectValue>{{ periodLabel }}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem v-for="p in PERIODS_S" :key="p" :value="String(p)">
            {{ p }} {{ t('system_unit_second') }}
          </SelectItem>
        </SelectContent>
      </Select>
    </div>

    <Card>
      <CardHeader><CardTitle>{{ t('system_cpu') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div class="grid gap-2 sm:grid-cols-2">
          <div>{{ t('system_temperature') }} : <span data-system-temperature>{{ temperature }}</span></div>
          <div>{{ t('system_frequency') }} : <span data-system-frequency>{{ frequency }}</span></div>
          <div>{{ t('system_cores') }} : <span data-system-cores>{{ number(state?.cpus) }}</span></div>
        </div>
        <!-- L'utilisation sort de la grille des trois autres métriques pour
             tenir sa propre ligne, juste au-dessus de sa barre : c'est ce qui
             la label. Dans la grille, elle atterrissait en deuxième colonne,
             à côté du number de cœurs, et la barre pleine largeur en dessous
             n'annonçait plus ce qu'elle mesurait. Même forme que Mémoire et
             Stockage : une ligne de text, puis sa barre. -->
        <div>
          {{ t('system_cpu_usage') }} :
          <span data-system-cpu-usage :class="cpuAlerting ? 'font-medium text-destructive' : undefined">
            {{ usageText }}
          </span>
        </div>
        <!-- Barre toujours présente, à zéro tant que le pourcentage est
             inconnu : elle apparaissait sinon d'un coup au deuxième sondage,
             en poussant la mise en page. Le risque de lire « 0 % » dans une
             barre vide est couvert par la ligne au-dessus, qui displayed « — »
             et non « 0 % » jusqu'à ce qu'un delta soit calculable. -->
        <div data-system-cpu-bar class="h-2 w-full rounded bg-muted">
          <div
            class="h-2 rounded"
            :class="cpuAlerting ? 'bg-destructive' : 'bg-primary'"
            :style="{ width: `${cpuWidth}%` }"
          />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_memory') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-memory>
          {{ usage(state?.memory, 'mb') }}
          <span v-if="state?.memory" class="text-muted-foreground">({{ usedPercent(state.memory) }} %)</span>
        </div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${usedPercent(state?.memory)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle class="flex items-baseline gap-2">
          {{ t('system_history') }}
          <span data-system-history-span class="text-xs font-normal text-muted-foreground">
            {{ t('system_history_span', { minutes: windowMinutes }) }}
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <!-- Le graphe est **toujours** rendu, vide tant qu'il n'y a step deux
             échantillons. Un message d'wait à sa place faisait sauter la
             mise en page au deuxième sondage, le text cédant d'un coup à une
             figure de 96 px. Rien à ruser pour l'obtenir : `sparklinePath`
             rend une chaîne vide sous deux points, et un `d` vide est un
             `<path>` invisible — c'est écrit dans son contract.

             `relative` : ancre le popin de survol au graphe, step à la carte
             entière. -->
        <div class="relative">
          <!-- `preserveAspectRatio="none"` étire le repère à la largeur
               disponible ; `vector-effect` empêche l'épaisseur du trait
               d'être étirée avec lui. Événements *pointer*, step *mouse* :
               la page se consulte surtout au doigt, et `pointermove` seul
               couvre déjà le survol souris et le glisser tactile. Pas de
               `touch-action: none` ici : ça bloquerait le défilement
               vertical de la page au-dessus du graphe sur un téléphone. -->
          <svg
            data-system-history
            :viewBox="`0 0 ${WIDTH} ${HEIGHT}`"
            preserveAspectRatio="none"
            class="h-24 w-full"
            role="img"
            :aria-label="t('system_history')"
            @pointermove="hoverPointer"
            @pointerdown="hoverPointer"
            @pointerleave="endHover"
            @pointercancel="endHover"
          >
            <!-- Repères de minute, dessinés **avant** les courbes pour passer
                 dessous : ce sont des jalons, step des données. Une encoche
                 sur le bas du cadre, sans text — l'échelle exacte est
                 annoncée une fois pour toutes par le libellé de la carte, et
                 la valeur d'un instant précis se lit au survol. -->
            <line
              v-for="(x, i) in ticks"
              :key="`repere-${i}`"
              data-system-history-tick
              :x1="x"
              :x2="x"
              :y1="HEIGHT - TICK_HEIGHT"
              :y2="HEIGHT"
              class="text-muted-foreground/60"
              stroke="currentColor"
              stroke-width="1"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="cpuPath"
              class="text-primary"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="ramPath"
              class="text-muted-foreground"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <!-- Troisième courbe distinguée par la color seule, sans
                 pointillé : `destructive` est la seule teinte garantie
                 distincte de `primary` et de `muted-foreground` dans les 42
                 presets du kit. Elle ne signale step une alerte ici — c'est la
                 color d'une série, et la légende dit laquelle. -->
            <path
              data-system-history-temp
              :d="tempPath"
              class="text-destructive"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <!-- Trait de survol seul, step de point par série : un
                 `<circle>` dans un viewBox étiré par
                 `preserveAspectRatio="none"` se dessinerait en ellipse, step
                 en cercle. Le trait plus les valeurs du popin répondent à
                 la demande sans ce défaut — ne step « corriger » en ajoutant
                 des cercles. -->
            <line
              v-if="hoverLineX !== null"
              data-system-history-line
              :x1="hoverLineX"
              :x2="hoverLineX"
              y1="0"
              :y2="HEIGHT"
              class="text-muted-foreground"
              stroke="currentColor"
              stroke-width="1"
              vector-effect="non-scaling-stroke"
            />
          </svg>
          <!-- `pointer-events-none` : le popin suit le pointeur sans lui
               jamais faire écran, sans quoi il capterait les événements
               dont il dépend. -->
          <div
            v-if="hoveredSample && popoverStyle"
            data-system-history-popin
            class="pointer-events-none absolute top-0 min-w-[100px] rounded-md border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-md"
            :style="popoverStyle"
          >
            <div>{{ new Date(hoveredSample.t).toLocaleTimeString() }}</div>
            <div class="text-primary">{{ t('system_cpu') }} {{ Math.round(hoveredSample.cpu) }} %</div>
            <div class="text-muted-foreground">{{ t('system_memory') }} {{ Math.round(hoveredSample.ram) }} %</div>
            <div v-if="hoveredSample.temp !== null" class="text-destructive">
              {{ t('system_temperature') }} {{ hoveredSample.temp.toFixed(1) }} °C
            </div>
          </div>
        </div>
        <!-- `—` et non « 0 % » sans échantillon : même convention que la
             lecture du CPU plus haut, pour ne step annoncer une mesure qu'on
             n'a step encore. -->
        <p data-system-history-legend class="mt-2 flex gap-4 text-xs">
          <span class="text-primary">
            {{ t('system_cpu') }} {{ last ? `${Math.round(last.cpu)} %` : NOTHING }}
          </span>
          <span class="text-muted-foreground">
            {{ t('system_memory') }} {{ last ? `${Math.round(last.ram)} %` : NOTHING }}
          </span>
          <!-- Annoncée d'après `state` et non d'après le last échantillon :
               l'existence d'une sonde est connue dès le premier sondage, donc
               la légende ne gagne step une colonne en cours de route. La valeur,
               elle, vient bien de l'échantillon, comme les deux autres. -->
          <span v-if="state?.temperature_c != null" class="text-destructive">
            {{ t('system_temperature') }}
            {{ last?.temp != null ? `${last.temp.toFixed(1)} °C` : NOTHING }}
          </span>
        </p>
        <!-- Disponible dès le premier sondage, contrairement au delta CPU :
             une figure moyennée dans le temps n'a step besoin de deux
             mesures. -->
        <p class="mt-2 text-xs text-muted-foreground">
          {{ t('system_loadavg') }} : <span data-system-load>{{ load }}</span>
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_storage') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-disk>{{ usage(state?.disk, 'gb') }}</div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${usedPercent(state?.disk)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_device') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div class="grid gap-2 sm:grid-cols-2">
          <div>{{ t('system_hostname') }} : <span data-system-hostname>{{ text(state?.hostname) }}</span></div>
          <div>{{ t('system_ip') }} : <span data-system-ip>{{ text(state?.ip) }}</span></div>
          <div>{{ t('system_os') }} : <span data-system-os>{{ text(state?.os) }}</span></div>
          <div>{{ t('system_kernel') }} : <span data-system-kernel>{{ text(state?.kernel) }}</span></div>
          <div>{{ t('system_version') }} : <span data-system-version>{{ text(state?.version) }}</span></div>
          <!-- La voltage remonte ici, en face de la version, pour que les deux
               durées de fonctionnement se retrouvent côte à côte sur la ligne
               suivante : ce sont elles qu'on lit ensemble (« la machine tourne
               depuis X, le service depuis Y »), et la grille à deux colonnes
               les séparait. -->
          <div>
            {{ t('system_voltage') }} :
            <span data-system-under-voltage :class="{ 'text-destructive': state?.under_voltage === true }">
              {{ voltage }}
            </span>
            <!-- Bouton d'aide, step un text déplié ici : cette cellule vit
                 dans la grille à deux colonnes dont on avait justement
                 **sorti** la phrase de conseil (`system_under_voltage`,
                 sous la grille ci-dessous) pendant le chantier système,
                 parce qu'un text long y débordait de sa cellule. L'aide est
                 plus longue encore que ce conseil, elle n'a donc step plus sa
                 place ici — d'où la popin plutôt qu'un paragraphe en place.
                 `size="icon-xs"` : assez petit pour rester un simple « (?) »
                 accolé au libellé, step un bouton qui rivalise avec lui. -->
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              data-system-voltage-help
              :aria-label="t('system_voltage_help')"
              @click="voltageHelpOpen = true"
            >
              ?
            </Button>
          </div>
          <div>{{ t('system_uptime') }} : <span data-system-uptime>{{ duration(state?.uptime_s) }}</span></div>
          <div>
            {{ t('system_service_uptime') }} :
            <span data-system-service-uptime>{{ duration(state?.service_uptime_s) }}</span>
          </div>
        </div>
        <!-- Un seul endroit pour l'état (la ligne ci-dessus, courte : « Sous-voltage »
             ou « Nominale »), un seul pour le conseil qui l'accompagne — et
             ce conseil n'existe que quand il s'applique. Avant, la phrase
             complète vivait dans la grille elle-même : deux-points doublés
             (« Tension d'alimentation : Sous-voltage détectée : vérifier
             l'alimentation. ») et un text qui débordait de sa cellule à deux
             colonnes. -->
        <p
          v-if="state?.under_voltage === true"
          data-system-under-voltage-avis
          role="status"
          class="text-destructive"
        >
          {{ t('system_under_voltage') }}
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_power') }}</CardTitle></CardHeader>
      <CardContent class="space-y-3">
        <p v-if="inProgress" data-power-progress aria-live="polite" class="text-sm text-muted-foreground">
          {{ currentMessage }}
        </p>
        <p
          v-else-if="state && (!state.can_power_off || !state.can_reboot)"
          data-power-unavailable
          class="text-sm text-muted-foreground"
        >
          {{ state.logind_reachable ? t('system_power_unavailable') : t('system_power_no_logind') }}
        </p>
        <div class="flex flex-wrap gap-2">
          <Button
            variant="destructive"
            data-power-poweroff
            :disabled="!!inProgress || !state?.can_power_off"
            @click="dialog = 'poweroff'"
          >
            {{ t('system_poweroff') }}
          </Button>
          <Button
            variant="destructive"
            data-power-reboot
            :disabled="!!inProgress || !state?.can_reboot"
            @click="dialog = 'reboot'"
          >
            {{ t('system_reboot') }}
          </Button>
          <Button
            variant="outline"
            data-power-restart
            :disabled="!!inProgress"
            @click="dialog = 'restart-service'"
          >
            {{ t('system_restart_service') }}
          </Button>
        </div>
      </CardContent>
    </Card>

    <!-- `data-logs-card` et step seulement `data-log-line` : la carte doit être
         repérable même quand le log est vide, sans quoi le journey de bout
         en bout ne saurait step distinguer « aucune erreur » de « carte
         disparue ». -->
    <Card data-logs-card>
      <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2">
        <ul class="space-y-1 font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in cardLogs" :key="i" data-log-line>{{ l }}</li>
        </ul>
        <!-- Offert dès la première erreur, et non seulement quand la carte
             déborde. Signalé à l'usage : réservé au log long, le filtre ne
             se découvrait qu'au moment où il y a trop à lire pour explorer
             l'écran. Il ne disparaît que sur un log vide, où il n'y aurait
             rien à ouvrir. -->
        <Button
          v-if="logs.length"
          variant="outline"
          size="sm"
          data-logs-all
          @click="openErrors"
        >
          {{ t('system_errors_all', { count: logs.length }) }}
        </Button>
      </CardContent>
    </Card>

    <!-- Popin des erreurs : `Dialog` du kit, comme l'aide sur la sous-voltage
         et le dialog d'alimentation, et rendue comme elles dans un portail —
         son contenu vit donc dans `document.body`, ce que les tests savent.
         Le compteur tient dans la `DialogDescription` : il décrit bien le
         dialog, et l'y mettre lui donne au passage son text d'accessibilité. -->
    <Dialog v-model:open="errorsOpen">
      <!-- Bien plus large que les autres popins, et c'est le seul cas qui le
           justifie : celles-ci portent une phrase, celle-ci porte des lignes de
           log. Le `DialogContent` du kit se cale a `sm:max-w-lg` (512 px),
           ou une ligne de log se replie trois ou quatre fois et devient
           illisible. Ici on prend l'ecran : 95 % de la window, borne a 1920 px
           pour qu'un ecran tres large n'etale step une ligne sur deux metres.

           Plus large que le `max-w-5xl` de la page elle-meme, donc, et
           volontairement : la page est un document qui se lit, ce dialog est
           un outil de diagnostic qui se scrute. -->
      <DialogContent class="sm:max-w-[min(95vw,120rem)]">
        <DialogHeader>
          <DialogTitle>{{ t('system_errors_title') }}</DialogTitle>
          <DialogDescription data-logs-count>
            {{ filteredLogs.length }} / {{ logs.length }}
          </DialogDescription>
        </DialogHeader>
        <Input
          v-model="errorsQuery"
          data-logs-filter
          :placeholder="t('system_errors_filter')"
        />
        <!-- `whitespace-pre-wrap` : une ligne de log aligne ses fields avec
             des espaces, que le rendu HTML par defaut reduit a un seul — la
             colonne du niveau et celle de la cible se retrouvaient decalees
             d'une ligne a l'autre. Le repli reste autorise (`pre-wrap` et non
             `pre`) : une ligne longue doit rester lisible sans defilement
             horizontal.

             70vh plutot que 60 : le dialog est le seul endroit qui montre plus
             que les dernieres lignes, autant qu'il en montre. -->
        <ul
          class="max-h-[70vh] space-y-1 overflow-y-auto font-mono text-xs whitespace-pre-wrap text-muted-foreground"
        >
          <li v-for="(l, i) in filteredLogs" :key="i" data-logs-dialog-line>{{ l }}</li>
        </ul>
        <p v-if="!filteredLogs.length" data-logs-empty class="text-sm text-muted-foreground">
          {{ t('system_errors_none') }}
        </p>
      </DialogContent>
    </Dialog>

    <!-- Popin d'aide sur la sous-voltage, indépendante du dialog
         d'alimentation ci-dessous : mêmes composants du kit (`Dialog` gère
         déjà le focus et l'échappement), aucun état ni contenu partagé. -->
    <Dialog v-model:open="voltageHelpOpen">
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ t('system_voltage_help_title') }}</DialogTitle>
          <DialogDescription>{{ t('system_voltage_help_body') }}</DialogDescription>
        </DialogHeader>
      </DialogContent>
    </Dialog>

    <!-- Un seul dialog pour les trois actions : le titre et la phrase de
         conséquence viennent de l'action en wait. -->
    <Dialog
      :open="dialog !== null"
      @update:open="(open: boolean) => { if (!open) dialog = null }"
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ dialog ? label(dialog) : '' }}</DialogTitle>
          <DialogDescription>{{ dialog ? consequence(dialog) : '' }}</DialogDescription>
        </DialogHeader>
        <div class="flex justify-end gap-2">
          <Button variant="outline" data-power-cancel @click="dialog = null">
            {{ t('system_cancel') }}
          </Button>
          <Button :variant="confirmVariant" data-power-confirm @click="confirm">
            {{ t('system_confirm') }}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  </div>
</template>
