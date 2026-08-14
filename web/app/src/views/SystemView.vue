<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, Select, SelectContent, SelectItem,
  SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { SystemPayload, SystemUsage } from '../types'
import { cheminSparkline } from './sparkline'

const { t } = useCatalog()
const etat = ref<SystemPayload | null>(null)
const indisponible = ref(false)

/**
 * Période de sondage, locale à la page : un confort de visualisation, pas un
 * réglage de l'appareil, donc ni `localStorage` ni `/api/settings` — cette
 * SPA ne garde aucun état côté navigateur, ses préférences vivent dans le
 * cœur. Elle revient donc à 5 s à chaque arrivée, comme l'historique qui
 * démarre vide. Le `Select` ne porte que des chaînes (voir `periode`
 * ci-dessous) ; la valeur réelle en millisecondes vit ici pour
 * `setInterval`.
 */
const periodeMs = ref(5000)
/** Options du sélecteur de période, en secondes. */
const PERIODES_S = [1, 2, 5, 10, 30] as const
/** Nombre d'échantillons conservés dans l'historique — voir `dureeFenetreMin`
 *  pour la fenêtre visible qui en découle à la période courante. */
const CAPACITE = 60
/** Repère du graphe, en unités de `viewBox`. */
const LARGEUR = 100
const HAUTEUR = 30
/** Valeur que la machine n'expose pas : un tiret cadratin plutôt qu'un 0,
 *  qui se lirait comme une mesure. */
const RIEN = '—'

const historique = ref<{ cpu: number; ram: number; t: number }[]>([])
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
/** Devient faux au démontage : empêche `demarrer()` de recréer un minuteur
 *  après coup (par ex. depuis `attendreRetour`, qui peut se terminer
 *  longtemps après que l'utilisateur a quitté la vue). */
let monte = true

/**
 * Compteurs jiffies du sondage précédent, pour calculer un delta — à part de
 * l'historique, qui n'a de sens qu'entre deux sondages consécutifs et non
 * comme une série à afficher.
 */
const precedentJiffies = ref<{ total: number; idle: number } | null>(null)

/** Dernière utilisation CPU calculée par `sonder`, indépendamment de
 *  l'historique : la carte CPU l'affiche dès qu'elle existe, sans attendre
 *  que la mémoire soit elle aussi lisible (condition propre à l'historique).
 *  Déclarée ici, à côté de `precedentJiffies` plutôt que près de son usage
 *  d'affichage plus bas : `sonder()` l'assigne, et ne compte que sur l'ordre
 *  d'exécution (premier appel via `onMounted`) pour que ça reste sûr — un
 *  futur appel plus impatient tomberait sur la zone morte temporelle d'un
 *  `const` déclaré après coup. */
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
 * Pourcentages retenus dans l'historique, avec l'horodatage du sondage (pour
 * un futur survol, pas encore affiché). `null` si l'un des deux manque : une
 * machine sans mémoire lisible, ou dont l'utilisation CPU n'est pas encore
 * calculable, garde un graphe vide plutôt qu'à moitié tracé. Une conséquence
 * à assumer : le premier échantillon exigeant lui-même un delta, le graphe
 * ne trace sa première ligne qu'au troisième sondage (deux pour produire un
 * échantillon, trois pour en avoir deux).
 */
function pourcentages(s: SystemPayload, cpu: number | null): { cpu: number; ram: number; t: number } | null {
  if (cpu == null || !s.memory || s.memory.total_kb === 0) return null
  return {
    cpu,
    ram: ((s.memory.total_kb - s.memory.available_kb) / s.memory.total_kb) * 100,
    t: Date.now(),
  }
}

/**
 * Sondage, là où le reste de la SPA reçoit du SSE, et c'est délibéré : le
 * flux `/api/player` publie un état que le cœur produit de toute façon,
 * alors que ces métriques n'existent que parce qu'on les demande. Les
 * pousser ferait travailler en permanence un appareil le plus souvent
 * inactif, pour personne. Le sondage s'arrête donc au démontage de la vue
 * et quand l'onglet passe en arrière-plan.
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
  const controleur = new AbortController()
  sondageEnVol = controleur
  try {
    const s = await api.get<SystemPayload>('/api/system', { signal: controleur.signal })
    etat.value = s
    indisponible.value = false
    const cpu = utilisationCpu(s)
    utilisationCpuActuelle.value = cpu
    const p = pourcentages(s, cpu)
    if (p) {
      historique.value.push(p)
      if (historique.value.length > CAPACITE) historique.value.shift()
    }
  } catch (e) {
    // Une annulation par `arreter()` (changement de période, démontage,
    // arrêt de l'appareil) rejette aussi le `fetch` : ce n'est pas un échec
    // du cœur, juste notre propre requête coupée court, donc pas de ligne
    // « indisponible » pour ça.
    if (controleur.signal.aborted) return
    indisponible.value = true
    console.warn('GET /api/system indisponible', e)
  } finally {
    if (sondageEnVol === controleur) sondageEnVol = null
  }
}

function demarrer() {
  // `enCours` : une action d'alimentation en cours a déjà arrêté le sondage
  // normal (voir `confirmer`) ; le laisser reprendre ici — par ex. au retour
  // de visibilité pendant un arrêt ou un redémarrage du service — afficherait
  // une erreur réseau alarmante sur un arrêt qui se déroule comme demandé,
  // ou sonderait en double avec `attendreRetour`. `document.hidden` : une
  // vue montée alors que l'onglet est déjà en arrière-plan ne doit pas
  // sonder avant le premier `visibilitychange`.
  if (!monte || document.hidden || enCours.value !== null || minuteur !== null) return
  void sonder()
  minuteur = setInterval(sonder, periodeMs.value)
}

function arreter() {
  if (minuteur !== null) {
    clearInterval(minuteur)
    minuteur = null
  }
  // Annule un sondage encore en vol : sans ça, un changement de période
  // laisserait une réponse plus ancienne atterrir après celle du nouveau
  // rythme et écraser `etat`/`precedentJiffies` avec des données périmées.
  if (sondageEnVol) {
    sondageEnVol.abort()
    sondageEnVol = null
  }
}

function visibilite() {
  if (document.hidden) arreter()
  else demarrer()
}

/**
 * Valeur de vue (chaîne, secondes) pour le sélecteur de période. Le
 * changement redémarre le sondage en repassant par `demarrer()` — sans le
 * contourner : c'est lui qui refuse de repartir pendant une action
 * d'alimentation en cours ou onglet caché, et cette garde doit rester unique.
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
 * afficherait sinon « 1 min » alors que le graphe trace encore une demi-heure
 * d'échantillons espacés de 30 s, et resterait faux pendant les 60 sondages
 * suivants. Repli sur la capacité théorique seulement tant qu'il n'y a rien
 * à mesurer (moins de deux échantillons).
 */
const dureeFenetreMin = computed(() => {
  const h = historique.value
  if (h.length >= 2) return Math.round((h.at(-1)!.t - h[0]!.t) / 60000)
  return Math.round((CAPACITE * (periodeMs.value / 1000)) / 60)
})

onMounted(() => {
  demarrer()
  document.addEventListener('visibilitychange', visibilite)
})
onUnmounted(() => {
  monte = false
  arreter()
  document.removeEventListener('visibilitychange', visibilite)
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
 * Ligne d'alimentation de la carte Appareil, toujours affichée — jamais
 * masquée derrière un `v-if` — pour distinguer trois situations que l'ancien
 * affichage confondait : aucune sonde (`null`, rendu « — » comme toute autre
 * métrique absente), une sonde qui rapporte une alimentation saine
 * (`false`), et une sous-tension réellement détectée (`true`). Une ligne
 * permanente qui passe au rouge se voit aussi bien qu'une bannière.
 *
 * Le mot est court (« Sous-tension », pas la phrase entière) : la phrase de
 * conseil (`system_under_voltage`) vit séparément, juste sous la grille, et
 * n'apparaît que lorsque l'alerte est active — un seul endroit pour l'état,
 * un seul pour le conseil, plutôt que les deux concaténés dans une cellule de
 * grille à deux colonnes qui les faisait déborder.
 */
const tension = computed(() => {
  if (etat.value?.under_voltage == null) return RIEN
  return etat.value.under_voltage ? t.value('system_voltage_low') : t.value('system_voltage_ok')
})
const dernier = computed(() => historique.value.at(-1) ?? null)
const cheminCpu = computed(() =>
  cheminSparkline(historique.value.map((h) => h.cpu), LARGEUR, HAUTEUR),
)
const cheminRam = computed(() =>
  cheminSparkline(historique.value.map((h) => h.ram), LARGEUR, HAUTEUR),
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
 * Traduit la position du pointeur en index d'échantillon : même mapping que
 * `cheminSparkline` (pas = `LARGEUR / (n - 1)`, inversé ici en repartant de
 * la fraction horizontale du rectangle réel de l'élément), pour que le
 * popin ne dérive jamais du tracé qu'il commente.
 */
function indexSurvol(event: PointerEvent): number {
  const rect = (event.currentTarget as Element).getBoundingClientRect()
  largeurGraphe.value = rect.width
  const n = historique.value.length
  const frac = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0
  return Math.min(n - 1, Math.max(0, Math.round(frac * (n - 1))))
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

/** Abscisse du trait de survol, en unités de `viewBox`. */
const xLigneSurvol = computed(() => {
  const n = historique.value.length
  if (survolIndex.value === null || n < 2) return null
  return survolIndex.value * (LARGEUR / (n - 1))
})

/** Échantillon pointé, pour les deux valeurs affichées dans le popin. */
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
 * dernières. Sur un tampon plein (60 échantillons) dans une carte étroite,
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
  const largeur = largeurGraphe.value
  if (largeur <= 0) {
    // Largeur pas encore mesurée : repli non borné plutôt qu'une division
    // par zéro — un cas qui ne devrait pas survenir en pratique, l'événement
    // pointeur qui produit `i` ayant déjà mesuré cette largeur au passage.
    return { left: `${(i / (n - 1)) * 100}%`, transform: 'translateX(-50%)' }
  }
  const centre = (i / (n - 1)) * largeur
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

/** Sondage rapproché pendant le redémarrage du service, et son plafond. */
const REPRISE_MS = 2000
const REPRISE_MAX_MS = 30000

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
 */
async function confirmer() {
  const action = dialogue.value
  if (!action) return
  dialogue.value = null
  enCours.value = action
  arreter()
  const uptimeAvant = etat.value?.service_uptime_s ?? null
  const err = await api.post('/api/system/power', { action })
  if (err) {
    // Refus de logind (règle polkit absente) ou cœur injoignable : rien ne
    // s'arrête, on rend la main.
    toast.error(err)
    enCours.value = null
    demarrer()
    return
  }
  if (action === 'restart-service') await attendreRetour(uptimeAvant)
}

/**
 * Le service redémarre : on sonde plus vite en ignorant les erreurs (il est
 * arrêté, c'est attendu). On ne le considère revenu que lorsque son uptime
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
 * `monte` dans la condition de boucle : si l'utilisateur a quitté la vue,
 * on cesse de sonder au tour suivant plutôt que de courir jusqu'au plafond
 * pour, à la fin, rappeler `demarrer()` sur une vue démontée — ce qui
 * recréerait un minuteur que plus personne ne pourrait jamais arrêter.
 */
async function attendreRetour(avant: number | null) {
  const t0 = Date.now()
  const limite = t0 + REPRISE_MAX_MS
  while (monte && Date.now() < limite) {
    await new Promise((r) => setTimeout(r, REPRISE_MS))
    try {
      // Le sondage est mis en course avec un délai : sans lui, une requête
      // qui se connecte mais ne répond jamais (Wi-Fi capricieux, socket à
      // moitié ouverte) bloquerait l'attente ici, indéfiniment, au-delà du
      // plafond de 30 s promis à l'utilisateur. La requête abandonnée reste
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
        // échec signalé 30 s trop tard n'est que du bruit. Ne pas
        // « corriger » cette asymétrie en symétrie.
        toast.success(t.value('system_restarted'))
        demarrer()
        return
      }
    } catch {
      // Service arrêté, ou sondage sans réponse : on réessaie jusqu'au plafond.
    }
  }
  if (!monte) return
  toast.error(t.value('system_restart_timeout'))
  enCours.value = null
  demarrer()
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
          <SelectValue />
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
      <CardContent class="grid gap-2 text-sm sm:grid-cols-2">
        <div>{{ t('system_temperature') }} : <span data-system-temperature>{{ temperature }}</span></div>
        <div>{{ t('system_frequency') }} : <span data-system-frequency>{{ frequence }}</span></div>
        <div>{{ t('system_cores') }} : <span data-system-cores>{{ nombre(etat?.cpus) }}</span></div>
        <div>{{ t('system_cpu_usage') }} : <span data-system-cpu-usage>{{ utilisationTexte }}</span></div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_memory') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-memory>{{ occupation(etat?.memory, 'mb') }}</div>
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
        <p
          v-if="historique.length < 2"
          data-system-history-empty
          class="text-sm text-muted-foreground"
        >
          {{ t('system_history_empty') }}
        </p>
        <template v-else>
          <!-- `relative` : ancre le popin de survol au graphe, pas à la
               carte entière. -->
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
            </div>
          </div>
          <p class="mt-2 flex gap-4 text-xs">
            <span class="text-primary">
              {{ t('system_cpu') }} {{ dernier ? Math.round(dernier.cpu) : 0 }} %
            </span>
            <span class="text-muted-foreground">
              {{ t('system_memory') }} {{ dernier ? Math.round(dernier.ram) : 0 }} %
            </span>
          </p>
        </template>
        <!-- Toujours visible, y compris pendant l'attente du graphe : une
             figure moyennée dans le temps est disponible dès le premier
             sondage, contrairement au delta CPU. -->
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
          <div>{{ t('system_uptime') }} : <span data-system-uptime>{{ duree(etat?.uptime_s) }}</span></div>
          <div>
            {{ t('system_service_uptime') }} :
            <span data-system-service-uptime>{{ duree(etat?.service_uptime_s) }}</span>
          </div>
          <div>
            {{ t('system_voltage') }} :
            <span data-system-under-voltage :class="{ 'text-destructive': etat?.under_voltage === true }">
              {{ tension }}
            </span>
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
          {{ t('system_power_unavailable') }}
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
