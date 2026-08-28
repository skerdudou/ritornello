<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, Input, Select, SelectContent, SelectItem,
  SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { PERIODES_S, useMetriques } from '../composables/useMetriques'
import type { DateFormat, LogsPayload, SettingsPayload, SystemPayload, SystemUsage } from '../types'
import { dateeLigne, filtreLignes } from './journal'
import { abscisses, cheminSparkline, reperesMinute } from './sparkline'

const { t } = useCatalog()
const {
  etat, indisponible, historique, utilisationCpuActuelle,
  periodeMs, periode, dureeFenetreMin, suspendre, reprendre,
} = useMetriques()

/**
 * Les dernières erreurs journalisées, relevées au montage et à chaque
 * ouverture de la popin (voir `releverJournal`).
 *
 * Elles vivaient sur la page Configuration ; leur place est ici, avec les
 * métriques, quand on cherche pourquoi l'appareil se comporte mal.
 *
 * Volontairement hors de `sonder()`, malgré la tentation : le sondage tient un
 * verrou « en vol » pour qu'un minuteur plus rapide que le réseau n'empile pas
 * deux relevés, et il calcule l'utilisation CPU par delta entre deux réponses.
 * Y greffer une seconde requête allonge la prise du verrou et change la cadence
 * observée — mesuré, quatre tests de cadence sont tombés. Rafraîchir la liste
 * demanderait donc son propre minuteur, pas un passager.
 */
const logs = ref<string[]>([])

/** Lignes d'erreur montrées directement dans la carte. Au-delà, la popin prend
 *  le relais : le tampon du cœur en garde 500, et les dérouler dans la page
 *  repousserait tout le reste hors de l'écran. */
const LOGS_CARTE = 8
const erreursOuvertes = ref(false)
const requeteErreurs = ref('')
/**
 * Les réglages d'écriture du temps, relevés une fois au montage.
 *
 * Une requête de plus sur cette page, et c'est le prix juste : les valeurs par
 * défaut suffisent tant qu'elle n'a pas répondu, et un échec laisse le journal
 * daté par défaut plutôt que de le priver de dates.
 */
const horloge = ref<{ date_format: DateFormat; clock_24h: boolean }>({
  date_format: 'day_month_year',
  clock_24h: true,
})

/**
 * Les lignes réécrites dans le format réglé, **avant** le filtre : ce qu'on
 * cherche est ce qu'on voit, donc une recherche sur « 14:03 » doit porter sur
 * l'heure affichée et non sur l'UTC que le cœur a écrit.
 */
const logsDates = computed(() =>
  logs.value.map((l) => dateeLigne(l, horloge.value.date_format, horloge.value.clock_24h)),
)
const logsCarte = computed(() => logsDates.value.slice(0, LOGS_CARTE))
const logsFiltres = computed(() => filtreLignes(logsDates.value, requeteErreurs.value))

/**
 * Relève le journal : au montage, et à chaque ouverture de la popin.
 *
 * Un geste utilisateur, donc toujours hors du sondage périodique — voir le
 * commentaire de `logs` : `sonder()` tient un verrou « en vol » et calcule un
 * delta CPU entre deux réponses, et y greffer une seconde requête change la
 * cadence observée (mesuré, quatre tests de cadence sont tombés).
 *
 * Son propre `.catch` : un journal indisponible ne doit pas priver
 * l'utilisateur des métriques, ni l'inverse. Un échec laisse la liste
 * précédente en place plutôt que de la vider — même convention que `reload`
 * de `useCatalog`.
 */
async function releverJournal(): Promise<void> {
  const j = await api.get<LogsPayload>('/api/logs').catch(() => null)
  if (j) logs.value = j.lines ?? []
}

function ouvrirErreurs(): void {
  // Filtre remis à zéro : une popin qui s'ouvre montre tout. Garder la requête
  // précédente la ferait rouvrir sur une liste tronquée, et le champ qui
  // l'explique est en haut du dialogue, pas sous les yeux de qui vient de
  // cliquer le bouton.
  requeteErreurs.value = ''
  erreursOuvertes.value = true
  void releverJournal()
}

/** Repère du graphe, en unités de `viewBox`. */
const LARGEUR = 100
const HAUTEUR = 30
/** Valeur que la machine n'expose pas : un tiret cadratin plutôt qu'un 0,
 *  qui se lirait comme une mesure. */
const RIEN = '—'

/** Devient faux au démontage. Ne garde plus que `attendreRetour` : sa boucle
 *  de sondage rapproché s'arrête au tour suivant, et son message d'échec ne
 *  s'affiche plus une fois la vue quittée. Le sondage régulier, lui, vit dans
 *  le store et ne dépend plus de cet indicateur — `demarrer()` ne le consulte
 *  pas. */
let monte = true

/**
 * Libellé du déclencheur, calculé ici plutôt que laissé à `SelectValue` sans
 * contenu : celui-ci affiche le texte de l'option sélectionnée **tel que
 * capturé au montage**, or le catalogue arrive après (chargement asynchrone
 * partagé, voir `useCatalog`). Le reste de la page se corrige tout seul quand
 * il arrive, `t` étant une computed — mais ce texte-là restait figé sur
 * « 5 system_unit_second », clé brute comprise. Une expression ici est relue
 * à chaque rendu, donc immunisée contre cette capture.
 */
const etiquettePeriode = computed(
  () => `${periodeMs.value / 1000} ${t.value('system_unit_second')}`,
)

// Le sondage des métriques n'est pas amorcé ici : il l'est une fois pour toute
// la SPA par `App.vue`. Ne reste au montage que le journal — hors du sondage
// périodique, mais pas pour autant relevé une seule fois dans la vie de la vue :
// l'ouverture de la popin le relève à nouveau (voir `releverJournal`).
onMounted(() => {
  void releverJournal()
  // Son propre `.catch` : des réglages injoignables ne doivent pas priver la
  // page de ses métriques ni de son journal.
  void api
    .get<SettingsPayload>('/api/settings')
    .then((r) => {
      horloge.value = { date_format: r.date_format, clock_24h: r.clock_24h }
    })
    .catch(() => {})
})
onUnmounted(() => {
  monte = false
})

// « °C » et « MHz » ne sont pas traduits : ce sont des symboles SI,
// identiques dans les deux langues — contrairement à Mo/MB et j/d.
const temperature = computed(() =>
  etat.value?.temperature_c == null ? RIEN : `${etat.value.temperature_c.toFixed(1)} °C`,
)
const frequence = computed(() =>
  etat.value?.cpu_mhz == null ? RIEN : `${etat.value.cpu_mhz} MHz`,
)
const charge = computed(() =>
  etat.value?.load ? etat.value.load.map((v) => v.toFixed(2)).join(' · ') : RIEN,
)
const utilisationTexte = computed(() =>
  utilisationCpuActuelle.value == null ? RIEN : `${Math.round(utilisationCpuActuelle.value)} %`,
)
/**
 * Seuil de mise en alerte de l'utilisation CPU. Strictement supérieur : 90 %
 * pile n'est pas encore une alerte.
 *
 * Comparé à la valeur **affichée** (arrondie), pas à la valeur brute : sinon
 * 90 < u <= 90,5 afficherait « 90 % » tout en déclenchant l'alerte, ce que ni
 * le libellé ni le commentaire ci-dessus ne laissent supposer.
 */
const SEUIL_ALERTE_CPU = 90
const cpuEnAlerte = computed(() => Math.round(utilisationCpuActuelle.value ?? 0) > SEUIL_ALERTE_CPU)
/**
 * Largeur de la barre. Passée par une computed plutôt qu'inline : le gabarit
 * n'a pas à réduire un `number | null` derrière son `v-if`, ce que la
 * vérification de types ne suit pas toujours à travers cette frontière.
 */
const largeurCpu = computed(() => Math.round(utilisationCpuActuelle.value ?? 0))
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
 * train de se produire), et une sous-tension réellement détectée à
 * l'instant (`under_voltage === true`, qui l'emporte sur l'antécédent —
 * inutile de dire « déjà vu » quand c'est en train de se reproduire). Une
 * ligne permanente qui passe au rouge se voit aussi bien qu'une bannière.
 *
 * Le mot est court (« Sous-tension », pas la phrase entière) : la phrase de
 * conseil (`system_under_voltage`) vit séparément, juste sous la grille, et
 * n'apparaît que lorsque l'alerte **instantanée** est active — un seul
 * endroit pour l'état, un seul pour le conseil, plutôt que les deux
 * concaténés dans une cellule de grille à deux colonnes qui les faisait
 * déborder. Le nouvel état, lui, ne déclenche pas cette phrase : il ne dit
 * rien à faire dans l'instant, seulement ce qui s'est déjà produit — ce que
 * l'aide (le bouton `(?)` ci-dessous) explique, sans répéter l'alerte.
 */
const tension = computed(() => {
  if (etat.value?.under_voltage == null) return RIEN
  if (etat.value.under_voltage) return t.value('system_voltage_low')
  if (etat.value.under_voltage_since_boot) return t.value('system_voltage_since_boot')
  return t.value('system_voltage_ok')
})

/** Ouverture de la popin d'aide sur la sous-tension (voir le bouton `(?)`
 *  dans le gabarit) : un état local à la vue, comme `dialogue` pour les
 *  actions d'alimentation, mais volontairement distinct — les deux popins
 *  n'ont rien en commun à part le composant `Dialog` du kit. */
const aideTensionOuverte = ref(false)
const dernier = computed(() => historique.value.at(-1) ?? null)
/**
 * Abscisses partagées par tout ce qui se place sur le graphe : les trois
 * tracés, le trait de survol et le calage du popin. Une seule source, pour
 * qu'aucun d'eux ne puisse dériver des autres.
 */
const abscissesGraphe = computed(() =>
  abscisses(historique.value.map((h) => h.t), LARGEUR),
)
const cheminCpu = computed(() =>
  cheminSparkline(historique.value.map((h) => h.cpu), abscissesGraphe.value, HAUTEUR),
)
const cheminRam = computed(() =>
  cheminSparkline(historique.value.map((h) => h.ram), abscissesGraphe.value, HAUTEUR),
)
/**
 * Tracé de la température, en °C sur le **même axe 0-100** que les deux
 * pourcentages : les °C d'un Pi vivent dans cette plage (throttle à 80-85), la
 * mi-hauteur se lit donc « 50 °C » sans second repère, et `cheminSparkline`
 * borne déjà à 0-100 — une machine à plus de 100 °C s'aplatirait en haut du
 * cadre, ce qui est le moindre de ses problèmes. C'est la légende qui porte
 * l'unité, et c'est elle qui rend un axe mixte honnête.
 *
 * Une valeur manquante ouvre un **trou** dans le tracé plutôt que d'effacer
 * la courbe entière ou de recopier la dernière température connue par-dessus
 * — voir le contrat de `cheminSparkline`, qui accepte directement des `null`
 * pour ça. L'ancienne version effaçait tout à la moindre lecture manquante,
 * au motif que les trois tracés, le trait de survol et le popin partagent un
 * seul jeu d'abscisses (`abscissesGraphe`) et qu'une série plus courte
 * dériverait des autres ; ce motif ne tenait que pour une série *tronquée*
 * (des valeurs retirées, donc décalées d'un rang). Un trou, lui, garde
 * chaque température présente sur sa propre abscisse — celle de son
 * horodatage, exactement comme dans les deux autres courbes — donc rien ne
 * dérive. Une machine sans sonde n'a toujours aucune courbe (toutes les
 * valeurs sont `null`), et un trou passager n'efface plus que le segment
 * concerné, pas les vingt minutes ou les deux heures d'historique qui
 * l'entourent.
 */
const cheminTemp = computed(() =>
  cheminSparkline(historique.value.map((h) => h.temp), abscissesGraphe.value, HAUTEUR),
)

/** Hauteur des repères de minute, en unités de `viewBox` : une encoche sur le
 *  bas du cadre, assez courte pour ne pas croiser les courbes. */
const HAUTEUR_REPERE = 4
/** Abscisses des repères de minute (voir `reperesMinute`). */
const reperes = computed(() =>
  reperesMinute(historique.value.map((h) => h.t), LARGEUR),
)

/** Index de la colonne survolée dans `historique`, `null` si le pointeur
 *  n'est pas sur le graphe. */
const survolIndex = ref<number | null>(null)

/** Largeur en pixels du graphe, mesurée au dernier événement pointeur : sert
 *  à borner la position du popin en pixels réels (voir `stylePopin`), plutôt
 *  qu'en pourcentage du conteneur — un pixel se borne directement, un
 *  pourcentage demanderait de connaître par avance la largeur du popin
 *  rapportée à celle, variable, de la carte. */
const largeurGraphe = ref(0)

/**
 * Traduit la position du pointeur en index d'échantillon : l'échantillon dont
 * l'abscisse est **la plus proche** du pointeur.
 *
 * Le calcul ne peut plus être un simple arrondi de rang (`frac × (n - 1)`) :
 * les points ne sont plus équidistants depuis qu'ils se placent à leur
 * horodatage, donc un rang proportionnel ne désigne plus la colonne qu'on voit
 * sous le curseur. La recherche part des mêmes abscisses que le tracé, ce qui
 * garantit par construction que le popin ne dérive pas de la courbe qu'il
 * commente. Boucle linéaire sur 240 points au plus, à chaque `pointermove` :
 * hors de portée de tout budget.
 */
function indexSurvol(event: PointerEvent): number {
  const rect = (event.currentTarget as Element).getBoundingClientRect()
  largeurGraphe.value = rect.width
  const frac = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0
  const cible = Math.min(1, Math.max(0, frac)) * LARGEUR
  let plusProche = 0
  let meilleureDistance = Number.POSITIVE_INFINITY
  // `<=` et non `<` : à distance égale — pointeur exactement à cheval entre
  // deux colonnes — c'est la colonne de droite qui gagne. Ce départage n'est
  // pas un détail d'implémentation mais le comportement qu'épinglait déjà le
  // test de l'arrondi, `Math.round` arrondissant les demis vers le haut. Le
  // changer silencieusement en passant du rang à l'abscisse aurait été une
  // régression invisible à l'œil.
  //
  // `forEach` plutôt qu'une boucle indexée : il livre l'abscisse elle-même, là
  // où un `xs[i]` demanderait de traiter un `undefined` que la longueur du
  // tableau exclut déjà.
  abscissesGraphe.value.forEach((x, i) => {
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
 * affiche le popin dès l'appui sur un écran tactile (sans lui, un simple tap
 * sans mouvement ne déclencherait jamais `pointermove`).
 */
function survolPointeur(event: PointerEvent) {
  if (historique.value.length < 2) return
  survolIndex.value = indexSurvol(event)
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
function finSurvol() {
  survolIndex.value = null
}

/** Abscisse du trait de survol, en unités de `viewBox` : celle de
 *  l'échantillon pointé, lue dans `abscissesGraphe` et non recalculée depuis
 *  son rang — c'est ce qui le garde exactement sur la courbe. */
const xLigneSurvol = computed(() => {
  const i = survolIndex.value
  if (i === null || historique.value.length < 2) return null
  return abscissesGraphe.value[i] ?? null
})

/** Échantillon pointé, pour les trois valeurs affichées dans le popin (la
 *  température n'y figurant que si la machine en expose une). */
const echantillonSurvol = computed(() => {
  if (survolIndex.value === null) return null
  return historique.value[survolIndex.value] ?? null
})

/** Largeur figée du popin (voir la classe `min-w-` sur son élément) : sert à
 *  connaître son demi-encombrement pour le borner ci-dessous, sans dépendre
 *  du texte affiché. */
const LARGEUR_POPIN_PX = 100
const DEMI_LARGEUR_POPIN_PX = LARGEUR_POPIN_PX / 2

/**
 * Position horizontale du popin : toujours centré sur la colonne pointée
 * (translation -50 % constante), avec la position bornée en pixels plutôt
 * que la colonne pointée elle-même.
 *
 * L'ancien code ne bornait que les deux colonnes extrêmes (`i === 0` et
 * `i === n - 1`) en désactivant le centrage sur elles seules — un raisonnement
 * pensé pour deux colonnes qui débordent, alors que le débordement touche en
 * réalité une bande entière de colonnes proches des bords (toutes celles à
 * moins d'un demi-popin du bord de la carte), pas seulement les deux
 * dernières. Sur un tampon plein (240 échantillons) dans une carte étroite,
 * ça laissait déborder les popins des index 1 à 4 environ, et symétriquement
 * en fin de série — précisément ce que la borne existe pour empêcher.
 *
 * Bornage en pixels (`largeurGraphe`, mesurée au dernier pointeur) et non via
 * un `clamp()` CSS mêlant `%` et `calc()` : les deux rendraient exactement la
 * même chose dans un navigateur, mais un pixel se borne par un simple
 * `Math.min`/`Math.max`, sans dépendre d'un moteur CSS pour l'interpréter —
 * ce qui inclut celui, très limité, de l'environnement de test.
 */
const stylePopin = computed(() => {
  const n = historique.value.length
  const i = survolIndex.value
  if (i === null || n < 2) return null
  // Fraction lue dans les abscisses partagées, et non `i / (n - 1)` : les
  // colonnes ne sont plus équidistantes, et un popin calé sur le rang se
  // décalerait de la colonne qu'il commente dès que la période de sondage
  // change en cours de route.
  const fraction = (abscissesGraphe.value[i] ?? 0) / LARGEUR
  const largeur = largeurGraphe.value
  if (largeur <= 0) {
    // Largeur pas encore mesurée : repli non borné plutôt qu'une division
    // par zéro — un cas qui ne devrait pas survenir en pratique, l'événement
    // pointeur qui produit `i` ayant déjà mesuré cette largeur au passage.
    return { left: `${fraction * 100}%`, transform: 'translateX(-50%)' }
  }
  const centre = fraction * largeur
  const bordeSup = Math.max(largeur - DEMI_LARGEUR_POPIN_PX, DEMI_LARGEUR_POPIN_PX)
  const gauche = Math.min(Math.max(centre, DEMI_LARGEUR_POPIN_PX), bordeSup)
  return { left: `${gauche}px`, transform: 'translateX(-50%)' }
})

function texte(v: string | null | undefined): string {
  return v || RIEN
}

function nombre(v: number | null | undefined): string {
  return v == null ? RIEN : String(v)
}

/** « 512 / 976 Mo » : utilisé et total dans la même unité, traduite. */
function occupation(u: SystemUsage | null | undefined, unite: 'mb' | 'gb'): string {
  if (!u) return RIEN
  const diviseur = unite === 'mb' ? 1024 : 1024 * 1024
  const chiffre = (kb: number) =>
    unite === 'mb' ? String(Math.round(kb / diviseur)) : (kb / diviseur).toFixed(1)
  const suffixe = t.value(unite === 'mb' ? 'system_unit_mb' : 'system_unit_gb')
  return `${chiffre(u.total_kb - u.available_kb)} / ${chiffre(u.total_kb)} ${suffixe}`
}

function pourcentOccupe(u: SystemUsage | null | undefined): number {
  if (!u || u.total_kb === 0) return 0
  return Math.round(((u.total_kb - u.available_kb) / u.total_kb) * 100)
}

/** Au plus deux unités : « 3 j 4 h », « 4 h 12 min », « 12 min ». */
function duree(secondes: number | null | undefined): string {
  if (secondes == null) return RIEN
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

type ActionPower = 'poweroff' | 'reboot' | 'restart-service'

/** Sondage rapproché pendant l'attente d'un retour, quelle que soit l'action. */
const REPRISE_MS = 2000
/** Plafond d'attente pour la relance du **service** : systemd relance le
 *  process dans la seconde (`Restart=always`), 30 s couvrent largement un
 *  démarrage lent. */
const REPRISE_MAX_MS = 30000
/** Plafond d'attente pour un redémarrage de la **machine** : quatre fois plus,
 *  parce qu'un Pi ne repart pas comme un process — arrêt des services,
 *  amorçage du noyau, montages, réseau, puis seulement le service. De l'ordre
 *  de 20 à 40 s sur du matériel sain (non mesuré ici) ; 120 s laissent la
 *  marge d'une carte SD lente ou d'un `fsck` au passage, sans laisser
 *  l'utilisateur devant un message qui ne conclut jamais. */
const REPRISE_MAX_REBOOT_MS = 120_000

/** Action dont on attend la confirmation, et action en cours. */
const dialogue = ref<ActionPower | null>(null)
const enCours = ref<ActionPower | null>(null)

function libelle(a: ActionPower): string {
  if (a === 'poweroff') return t.value('system_poweroff')
  if (a === 'reboot') return t.value('system_reboot')
  return t.value('system_restart_service')
}

function consequence(a: ActionPower): string {
  if (a === 'poweroff') return t.value('system_confirm_poweroff')
  if (a === 'reboot') return t.value('system_confirm_reboot')
  return t.value('system_confirm_restart_service')
}

const messageEnCours = computed(() => {
  if (enCours.value === 'poweroff') return t.value('system_powering_off')
  if (enCours.value === 'reboot') return t.value('system_rebooting')
  if (enCours.value === 'restart-service') return t.value('system_restarting')
  return ''
})

/** Le bouton de confirmation n'est peint en « destructive » que pour les
 *  actions qui le sont réellement : la relance du service laisse l'appareil
 *  allumé, ce que sa propre phrase de conséquence promet. */
const variantConfirmation = computed(() => (dialogue.value === 'restart-service' ? 'default' : 'destructive'))

/**
 * Le cœur va disparaître : le sondage normal s'arrête avant l'envoi. Sans
 * cela, le sondage suivant échouerait et afficherait une erreur réseau
 * alarmante alors que l'arrêt se passe exactement comme demandé.
 *
 * Deux des trois actions attendent ensuite le retour, et une seule reste
 * suspendue : l'arrêt. Le redémarrage de la machine s'attend comme la relance
 * du service — plus longuement, voir `REPRISE_MAX_REBOOT_MS` — parce que
 * l'appareil revient et que l'onglet, lui, est resté ouvert. Le laisser
 * suspendu figerait le graphe de **toutes** les pages jusqu'au rechargement
 * complet, sans rien à l'écran pour l'expliquer : `enCours` est local à la vue
 * et disparaît avec elle, `indisponible` reste faux. Seul l'arrêt justifie la
 * suspension définitive, l'appareil ne revenant que par un geste physique.
 */
async function confirmer() {
  const action = dialogue.value
  if (!action) return
  dialogue.value = null
  enCours.value = action
  suspendre()
  const uptimeAvant = etat.value?.service_uptime_s ?? null
  const err = await api.post('/api/system/power', { action })
  if (err) {
    // Refus de logind (règle polkit absente) ou cœur injoignable : rien ne
    // s'arrête, on rend la main. Chemin banal sur cette machine, pas un cas
    // limite — une installation DietPi sans la règle polkit, ou avec
    // `systemd-logind` masqué, refuse le tout premier appel.
    toast.error(err)
    enCours.value = null
    reprendre()
    return
  }
  if (action === 'restart-service') {
    await attendreRetour(uptimeAvant, REPRISE_MAX_MS, 'system_restarted')
  } else if (action === 'reboot') {
    await attendreRetour(uptimeAvant, REPRISE_MAX_REBOOT_MS, 'system_device_restarted')
  }
}

/**
 * Le service — ou la machine entière — redémarre : on sonde plus vite en
 * ignorant les erreurs (il est arrêté, c'est attendu). Le plafond et le message
 * de succès arrivent en paramètres plutôt que d'être déduits de l'action ici :
 * la fonction n'a pas à connaître les trois actions de la page, et un plafond
 * nommé au point d'appel se lit avec la raison qui le motive.
 *
 * On ne le considère revenu que lorsque son uptime
 * est *inférieur à ce que l'ancien process afficherait maintenant* — et non
 * simplement inférieur à `avant` : juste après un redémarrage réussi,
 * `service_uptime_s` vaut très souvent 0, et rien ne peut jamais être
 * strictement inférieur à 0. Comparer à `avant + écoulé` (l'uptime que
 * l'ancien process, lui, continue d'accumuler pendant qu'on attend) reste
 * vrai même quand le process revenu affiche 0. Pas de marge ajoutée à ce
 * seuil : `Math.floor` ne peut que retarder l'acceptation d'une seconde,
 * alors qu'une marge ajoutée au seuil faciliterait l'acceptation et
 * pourrait faire passer l'*ancien* process pour un process redémarré — soit
 * exactement le bug que cette comparaison d'uptime existe pour empêcher.
 *
 * Le même test vaut pour un redémarrage de la machine, et un lecteur pourrait
 * en douter : c'est bien `service_uptime_s` qu'on compare, pas `uptime_s`, et
 * il repart de zéro avec la machine puisque le service redémarre avec elle. Un
 * redémarrage complet satisfait donc le seuil au moins aussi franchement qu'une
 * simple relance de service — il n'y a rien à adapter.
 *
 * `monte` dans la condition de boucle : si l'utilisateur a quitté la vue, on
 * cesse ce sondage rapproché au tour suivant plutôt que de courir jusqu'au
 * plafond pour une page que plus personne ne regarde. Ce qui suit la boucle,
 * en revanche, doit s'exécuter dans les deux cas : le sondage régulier vit
 * dans le store, partagé par toute la SPA, et le laisser suspendu figerait le
 * graphe de toutes les pages jusqu'au rechargement complet. Reprendre sur une
 * vue démontée n'est plus le danger que ce commentaire redoutait — le minuteur
 * survit de toute façon à chaque vue, c'est sa raison d'être. Seul le message
 * d'échec reste conditionné à `monte`.
 */
async function attendreRetour(avant: number | null, maxMs: number, cleSucces: string) {
  const t0 = Date.now()
  const limite = t0 + maxMs
  while (monte && Date.now() < limite) {
    await new Promise((r) => setTimeout(r, REPRISE_MS))
    try {
      // Le sondage est mis en course avec un délai : sans lui, une requête
      // qui se connecte mais ne répond jamais (Wi-Fi capricieux, socket à
      // moitié ouverte) bloquerait l'attente ici, indéfiniment, au-delà du
      // plafond promis à l'utilisateur. La requête abandonnée reste
      // en vol mais n'a plus d'effet : la boucle a déjà tourné la page.
      const s = await Promise.race([
        api.get<SystemPayload>('/api/system'),
        new Promise<never>((_, rejette) =>
          setTimeout(() => rejette(new Error('sondage sans réponse')), REPRISE_MS),
        ),
      ])
      const ecoule = Math.floor((Date.now() - t0) / 1000)
      if (avant === null || s.service_uptime_s < avant + ecoule) {
        etat.value = s
        enCours.value = null
        // Pas de garde sur `monte` ici, contrairement au message de délai
        // ci-dessous : c'est délibéré. Un succès annoncé après que
        // l'utilisateur a quitté la vue reste une information utile ; un
        // échec signalé bien trop tard n'est que du bruit. Ne pas
        // « corriger » cette asymétrie en symétrie.
        toast.success(t.value(cleSucces))
        reprendre()
        return
      }
    } catch {
      // Service arrêté, ou sondage sans réponse : on réessaie jusqu'au plafond.
    }
  }
  // Sortie par plafond **ou** par démontage : dans les deux cas le sondage doit
  // reprendre. Il est désormais partagé par toute la SPA, et un `return` sec sur
  // `!monte` le laisserait suspendu pour de bon — le graphe de chaque page figé,
  // sans rien à l'écran pour l'expliquer.
  //
  // Compromis vu et assumé, pas oublié : cette reprise est inconditionnelle,
  // donc une boucle restée d'une instance démontée peut, en se réveillant de
  // son sommeil de `REPRISE_MS`, reprendre une suspension qu'une action
  // d'alimentation *tout juste* confirmée venait de prendre. La fenêtre est
  // bornée à 2 s et le cas demande de quitter la vue pendant une attente puis
  // de reconfirmer aussitôt ; le remède propre est un jeton de suspension
  // plutôt qu'un booléen, et il est hors du périmètre ici. Entre ce risque-là
  // et un `suspendu` figé pour la vie de la page, c'est celui-ci qu'on prend.
  //
  // Faire attendre `reboot` sur `attendreRetour` élargit ce risque sur deux
  // plans, pas un seul : avant, seule la relance du service passait par cette
  // boucle, donc seule une reconfirmation de relance de service pendant son
  // attente pouvait le déclencher ; le redémarrage de la machine en ouvre un
  // second déclencheur. Et la période pendant laquelle quitter la vue peut
  // faire naître une telle boucle s'étire d'autant que son plafond : au plus
  // 30 s auparavant (`REPRISE_MAX_MS`), au plus 120 s désormais
  // (`REPRISE_MAX_REBOOT_MS`) pour un redémarrage confirmé puis abandonné.
  enCours.value = null
  reprendre()
  // Le message d'échec, lui, reste conditionnel : un échec signalé une ou deux
  // minutes après que l'utilisateur a quitté la vue n'est que du bruit.
  if (!monte) return
  toast.error(t.value('system_restart_timeout'))
}
</script>

<template>
  <div class="space-y-4">
    <p v-if="indisponible" data-system-unavailable class="text-sm text-destructive">
      {{ t('system_unavailable') }}
    </p>

    <!-- Pas de CardTitle associé au déclencheur ici (pas de Card autour) :
         aria-label obligatoire, même motif que les Select de ConfigView.vue. -->
    <div class="flex items-center gap-2">
      <span class="text-sm text-muted-foreground">{{ t('system_period') }}</span>
      <Select v-model="periode">
        <SelectTrigger data-system-period class="w-24" :aria-label="t('system_period')">
          <SelectValue>{{ etiquettePeriode }}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem v-for="p in PERIODES_S" :key="p" :value="String(p)">
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
          <div>{{ t('system_frequency') }} : <span data-system-frequency>{{ frequence }}</span></div>
          <div>{{ t('system_cores') }} : <span data-system-cores>{{ nombre(etat?.cpus) }}</span></div>
        </div>
        <!-- L'utilisation sort de la grille des trois autres métriques pour
             tenir sa propre ligne, juste au-dessus de sa barre : c'est ce qui
             la libelle. Dans la grille, elle atterrissait en deuxième colonne,
             à côté du nombre de cœurs, et la barre pleine largeur en dessous
             n'annonçait plus ce qu'elle mesurait. Même forme que Mémoire et
             Stockage : une ligne de texte, puis sa barre. -->
        <div>
          {{ t('system_cpu_usage') }} :
          <span data-system-cpu-usage :class="cpuEnAlerte ? 'font-medium text-destructive' : undefined">
            {{ utilisationTexte }}
          </span>
        </div>
        <!-- Barre toujours présente, à zéro tant que le pourcentage est
             inconnu : elle apparaissait sinon d'un coup au deuxième sondage,
             en poussant la mise en page. Le risque de lire « 0 % » dans une
             barre vide est couvert par la ligne au-dessus, qui affiche « — »
             et non « 0 % » jusqu'à ce qu'un delta soit calculable. -->
        <div data-system-cpu-bar class="h-2 w-full rounded bg-muted">
          <div
            class="h-2 rounded"
            :class="cpuEnAlerte ? 'bg-destructive' : 'bg-primary'"
            :style="{ width: `${largeurCpu}%` }"
          />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_memory') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-memory>
          {{ occupation(etat?.memory, 'mb') }}
          <span v-if="etat?.memory" class="text-muted-foreground">({{ pourcentOccupe(etat.memory) }} %)</span>
        </div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${pourcentOccupe(etat?.memory)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle class="flex items-baseline gap-2">
          {{ t('system_history') }}
          <span data-system-history-span class="text-xs font-normal text-muted-foreground">
            {{ t('system_history_span', { minutes: dureeFenetreMin }) }}
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <!-- Le graphe est **toujours** rendu, vide tant qu'il n'y a pas deux
             échantillons. Un message d'attente à sa place faisait sauter la
             mise en page au deuxième sondage, le texte cédant d'un coup à une
             figure de 96 px. Rien à ruser pour l'obtenir : `cheminSparkline`
             rend une chaîne vide sous deux points, et un `d` vide est un
             `<path>` invisible — c'est écrit dans son contrat.

             `relative` : ancre le popin de survol au graphe, pas à la carte
             entière. -->
        <div class="relative">
          <!-- `preserveAspectRatio="none"` étire le repère à la largeur
               disponible ; `vector-effect` empêche l'épaisseur du trait
               d'être étirée avec lui. Événements *pointer*, pas *mouse* :
               la page se consulte surtout au doigt, et `pointermove` seul
               couvre déjà le survol souris et le glisser tactile. Pas de
               `touch-action: none` ici : ça bloquerait le défilement
               vertical de la page au-dessus du graphe sur un téléphone. -->
          <svg
            data-system-history
            :viewBox="`0 0 ${LARGEUR} ${HAUTEUR}`"
            preserveAspectRatio="none"
            class="h-24 w-full"
            role="img"
            :aria-label="t('system_history')"
            @pointermove="survolPointeur"
            @pointerdown="survolPointeur"
            @pointerleave="finSurvol"
            @pointercancel="finSurvol"
          >
            <!-- Repères de minute, dessinés **avant** les courbes pour passer
                 dessous : ce sont des jalons, pas des données. Une encoche
                 sur le bas du cadre, sans texte — l'échelle exacte est
                 annoncée une fois pour toutes par le libellé de la carte, et
                 la valeur d'un instant précis se lit au survol. -->
            <line
              v-for="(x, i) in reperes"
              :key="`repere-${i}`"
              data-system-history-tick
              :x1="x"
              :x2="x"
              :y1="HAUTEUR - HAUTEUR_REPERE"
              :y2="HAUTEUR"
              class="text-muted-foreground/60"
              stroke="currentColor"
              stroke-width="1"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="cheminCpu"
              class="text-primary"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="cheminRam"
              class="text-muted-foreground"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <!-- Troisième courbe distinguée par la couleur seule, sans
                 pointillé : `destructive` est la seule teinte garantie
                 distincte de `primary` et de `muted-foreground` dans les 42
                 presets du kit. Elle ne signale pas une alerte ici — c'est la
                 couleur d'une série, et la légende dit laquelle. -->
            <path
              data-system-history-temp
              :d="cheminTemp"
              class="text-destructive"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <!-- Trait de survol seul, pas de point par série : un
                 `<circle>` dans un viewBox étiré par
                 `preserveAspectRatio="none"` se dessinerait en ellipse, pas
                 en cercle. Le trait plus les valeurs du popin répondent à
                 la demande sans ce défaut — ne pas « corriger » en ajoutant
                 des cercles. -->
            <line
              v-if="xLigneSurvol !== null"
              data-system-history-line
              :x1="xLigneSurvol"
              :x2="xLigneSurvol"
              y1="0"
              :y2="HAUTEUR"
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
            v-if="echantillonSurvol && stylePopin"
            data-system-history-popin
            class="pointer-events-none absolute top-0 min-w-[100px] rounded-md border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-md"
            :style="stylePopin"
          >
            <div>{{ new Date(echantillonSurvol.t).toLocaleTimeString() }}</div>
            <div class="text-primary">{{ t('system_cpu') }} {{ Math.round(echantillonSurvol.cpu) }} %</div>
            <div class="text-muted-foreground">{{ t('system_memory') }} {{ Math.round(echantillonSurvol.ram) }} %</div>
            <div v-if="echantillonSurvol.temp !== null" class="text-destructive">
              {{ t('system_temperature') }} {{ echantillonSurvol.temp.toFixed(1) }} °C
            </div>
          </div>
        </div>
        <!-- `—` et non « 0 % » sans échantillon : même convention que la
             lecture du CPU plus haut, pour ne pas annoncer une mesure qu'on
             n'a pas encore. -->
        <p data-system-history-legend class="mt-2 flex gap-4 text-xs">
          <span class="text-primary">
            {{ t('system_cpu') }} {{ dernier ? `${Math.round(dernier.cpu)} %` : RIEN }}
          </span>
          <span class="text-muted-foreground">
            {{ t('system_memory') }} {{ dernier ? `${Math.round(dernier.ram)} %` : RIEN }}
          </span>
          <!-- Annoncée d'après `etat` et non d'après le dernier échantillon :
               l'existence d'une sonde est connue dès le premier sondage, donc
               la légende ne gagne pas une colonne en cours de route. La valeur,
               elle, vient bien de l'échantillon, comme les deux autres. -->
          <span v-if="etat?.temperature_c != null" class="text-destructive">
            {{ t('system_temperature') }}
            {{ dernier?.temp != null ? `${dernier.temp.toFixed(1)} °C` : RIEN }}
          </span>
        </p>
        <!-- Disponible dès le premier sondage, contrairement au delta CPU :
             une figure moyennée dans le temps n'a pas besoin de deux
             mesures. -->
        <p class="mt-2 text-xs text-muted-foreground">
          {{ t('system_loadavg') }} : <span data-system-load>{{ charge }}</span>
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_storage') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-disk>{{ occupation(etat?.disk, 'gb') }}</div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${pourcentOccupe(etat?.disk)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_device') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div class="grid gap-2 sm:grid-cols-2">
          <div>{{ t('system_hostname') }} : <span data-system-hostname>{{ texte(etat?.hostname) }}</span></div>
          <div>{{ t('system_ip') }} : <span data-system-ip>{{ texte(etat?.ip) }}</span></div>
          <div>{{ t('system_os') }} : <span data-system-os>{{ texte(etat?.os) }}</span></div>
          <div>{{ t('system_kernel') }} : <span data-system-kernel>{{ texte(etat?.kernel) }}</span></div>
          <div>{{ t('system_version') }} : <span data-system-version>{{ texte(etat?.version) }}</span></div>
          <!-- La tension remonte ici, en face de la version, pour que les deux
               durées de fonctionnement se retrouvent côte à côte sur la ligne
               suivante : ce sont elles qu'on lit ensemble (« la machine tourne
               depuis X, le service depuis Y »), et la grille à deux colonnes
               les séparait. -->
          <div>
            {{ t('system_voltage') }} :
            <span data-system-under-voltage :class="{ 'text-destructive': etat?.under_voltage === true }">
              {{ tension }}
            </span>
            <!-- Bouton d'aide, pas un texte déplié ici : cette cellule vit
                 dans la grille à deux colonnes dont on avait justement
                 **sorti** la phrase de conseil (`system_under_voltage`,
                 sous la grille ci-dessous) pendant le chantier système,
                 parce qu'un texte long y débordait de sa cellule. L'aide est
                 plus longue encore que ce conseil, elle n'a donc pas plus sa
                 place ici — d'où la popin plutôt qu'un paragraphe en place.
                 `size="icon-xs"` : assez petit pour rester un simple « (?) »
                 accolé au libellé, pas un bouton qui rivalise avec lui. -->
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              data-system-voltage-help
              :aria-label="t('system_voltage_help')"
              @click="aideTensionOuverte = true"
            >
              ?
            </Button>
          </div>
          <div>{{ t('system_uptime') }} : <span data-system-uptime>{{ duree(etat?.uptime_s) }}</span></div>
          <div>
            {{ t('system_service_uptime') }} :
            <span data-system-service-uptime>{{ duree(etat?.service_uptime_s) }}</span>
          </div>
        </div>
        <!-- Un seul endroit pour l'état (la ligne ci-dessus, courte : « Sous-tension »
             ou « Nominale »), un seul pour le conseil qui l'accompagne — et
             ce conseil n'existe que quand il s'applique. Avant, la phrase
             complète vivait dans la grille elle-même : deux-points doublés
             (« Tension d'alimentation : Sous-tension détectée : vérifier
             l'alimentation. ») et un texte qui débordait de sa cellule à deux
             colonnes. -->
        <p
          v-if="etat?.under_voltage === true"
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
        <p v-if="enCours" data-power-progress aria-live="polite" class="text-sm text-muted-foreground">
          {{ messageEnCours }}
        </p>
        <p
          v-else-if="etat && (!etat.can_power_off || !etat.can_reboot)"
          data-power-unavailable
          class="text-sm text-muted-foreground"
        >
          {{ etat.logind_reachable ? t('system_power_unavailable') : t('system_power_no_logind') }}
        </p>
        <div class="flex flex-wrap gap-2">
          <Button
            variant="destructive"
            data-power-poweroff
            :disabled="!!enCours || !etat?.can_power_off"
            @click="dialogue = 'poweroff'"
          >
            {{ t('system_poweroff') }}
          </Button>
          <Button
            variant="destructive"
            data-power-reboot
            :disabled="!!enCours || !etat?.can_reboot"
            @click="dialogue = 'reboot'"
          >
            {{ t('system_reboot') }}
          </Button>
          <Button
            variant="outline"
            data-power-restart
            :disabled="!!enCours"
            @click="dialogue = 'restart-service'"
          >
            {{ t('system_restart_service') }}
          </Button>
        </div>
      </CardContent>
    </Card>

    <!-- `data-logs-card` et pas seulement `data-log-line` : la carte doit être
         repérable même quand le journal est vide, sans quoi le parcours de bout
         en bout ne saurait pas distinguer « aucune erreur » de « carte
         disparue ». -->
    <Card data-logs-card>
      <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2">
        <ul class="space-y-1 font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in logsCarte" :key="i" data-log-line>{{ l }}</li>
        </ul>
        <!-- Offert dès la première erreur, et non seulement quand la carte
             déborde. Signalé à l'usage : réservé au journal long, le filtre ne
             se découvrait qu'au moment où il y a trop à lire pour explorer
             l'écran. Il ne disparaît que sur un journal vide, où il n'y aurait
             rien à ouvrir. -->
        <Button
          v-if="logs.length"
          variant="outline"
          size="sm"
          data-logs-all
          @click="ouvrirErreurs"
        >
          {{ t('system_errors_all', { count: logs.length }) }}
        </Button>
      </CardContent>
    </Card>

    <!-- Popin des erreurs : `Dialog` du kit, comme l'aide sur la sous-tension
         et le dialogue d'alimentation, et rendue comme elles dans un portail —
         son contenu vit donc dans `document.body`, ce que les tests savent.
         Le compteur tient dans la `DialogDescription` : il décrit bien le
         dialogue, et l'y mettre lui donne au passage son texte d'accessibilité. -->
    <Dialog v-model:open="erreursOuvertes">
      <!-- Bien plus large que les autres popins, et c'est le seul cas qui le
           justifie : celles-ci portent une phrase, celle-ci porte des lignes de
           journal. Le `DialogContent` du kit se cale a `sm:max-w-lg` (512 px),
           ou une ligne de log se replie trois ou quatre fois et devient
           illisible. Ici on prend l'ecran : 95 % de la fenetre, borne a 1920 px
           pour qu'un ecran tres large n'etale pas une ligne sur deux metres.

           Plus large que le `max-w-5xl` de la page elle-meme, donc, et
           volontairement : la page est un document qui se lit, ce dialogue est
           un outil de diagnostic qui se scrute. -->
      <DialogContent class="sm:max-w-[min(95vw,120rem)]">
        <DialogHeader>
          <DialogTitle>{{ t('system_errors_title') }}</DialogTitle>
          <DialogDescription data-logs-count>
            {{ logsFiltres.length }} / {{ logs.length }}
          </DialogDescription>
        </DialogHeader>
        <Input
          v-model="requeteErreurs"
          data-logs-filter
          :placeholder="t('system_errors_filter')"
        />
        <!-- `whitespace-pre-wrap` : une ligne de journal aligne ses champs avec
             des espaces, que le rendu HTML par defaut reduit a un seul — la
             colonne du niveau et celle de la cible se retrouvaient decalees
             d'une ligne a l'autre. Le repli reste autorise (`pre-wrap` et non
             `pre`) : une ligne longue doit rester lisible sans defilement
             horizontal.

             70vh plutot que 60 : le dialogue est le seul endroit qui montre plus
             que les dernieres lignes, autant qu'il en montre. -->
        <ul
          class="max-h-[70vh] space-y-1 overflow-y-auto font-mono text-xs whitespace-pre-wrap text-muted-foreground"
        >
          <li v-for="(l, i) in logsFiltres" :key="i" data-logs-dialog-line>{{ l }}</li>
        </ul>
        <p v-if="!logsFiltres.length" data-logs-empty class="text-sm text-muted-foreground">
          {{ t('system_errors_none') }}
        </p>
      </DialogContent>
    </Dialog>

    <!-- Popin d'aide sur la sous-tension, indépendante du dialogue
         d'alimentation ci-dessous : mêmes composants du kit (`Dialog` gère
         déjà le focus et l'échappement), aucun état ni contenu partagé. -->
    <Dialog v-model:open="aideTensionOuverte">
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ t('system_voltage_help_title') }}</DialogTitle>
          <DialogDescription>{{ t('system_voltage_help_body') }}</DialogDescription>
        </DialogHeader>
      </DialogContent>
    </Dialog>

    <!-- Un seul dialogue pour les trois actions : le titre et la phrase de
         conséquence viennent de l'action en attente. -->
    <Dialog
      :open="dialogue !== null"
      @update:open="(ouvert: boolean) => { if (!ouvert) dialogue = null }"
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ dialogue ? libelle(dialogue) : '' }}</DialogTitle>
          <DialogDescription>{{ dialogue ? consequence(dialogue) : '' }}</DialogDescription>
        </DialogHeader>
        <div class="flex justify-end gap-2">
          <Button variant="outline" data-power-cancel @click="dialogue = null">
            {{ t('system_cancel') }}
          </Button>
          <Button :variant="variantConfirmation" data-power-confirm @click="confirmer">
            {{ t('system_confirm') }}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  </div>
</template>
