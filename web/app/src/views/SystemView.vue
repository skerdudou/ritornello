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
  try {
    const s = await api.get<SystemPayload>('/api/system')
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
    indisponible.value = true
    console.warn('GET /api/system indisponible', e)
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
    periodeMs.value = Number(v) * 1000
    arreter()
    demarrer()
  },
})

/** Fenêtre visible de l'historique, en minutes : `CAPACITE` échantillons à
 *  la période courante. Affichée pour ne pas laisser deviner : à 60
 *  échantillons, elle vaut numériquement la période en secondes (1 s → 1
 *  min, 30 s → 30 min), mais la formule reste écrite en toutes lettres plutôt
 *  que de s'appuyer sur cette coïncidence. */
const dureeFenetreMin = computed(() => (CAPACITE * (periodeMs.value / 1000)) / 60)

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
/** Dernière utilisation CPU calculée par `sonder`, indépendamment de
 *  l'historique : la carte CPU l'affiche dès qu'elle existe, sans attendre
 *  que la mémoire soit elle aussi lisible (condition propre à l'historique). */
const utilisationCpuActuelle = ref<number | null>(null)
const utilisationTexte = computed(() =>
  utilisationCpuActuelle.value == null ? RIEN : `${Math.round(utilisationCpuActuelle.value)} %`,
)
/**
 * Ligne d'alimentation de la carte Appareil, toujours affichée — jamais
 * masquée derrière un `v-if` — pour distinguer trois situations que l'ancien
 * affichage confondait : aucune sonde (`null`, rendu « — » comme toute autre
 * métrique absente), une sonde qui rapporte une alimentation saine
 * (`false`), et une sous-tension réellement détectée (`true`). Une ligne
 * permanente qui passe au rouge se voit aussi bien qu'une bannière, et
 * l'information n'existe plus qu'à un seul endroit.
 */
const tension = computed(() => {
  if (etat.value?.under_voltage == null) return RIEN
  return etat.value.under_voltage ? t.value('system_under_voltage') : t.value('system_voltage_ok')
})
const dernier = computed(() => historique.value.at(-1) ?? null)
const cheminCpu = computed(() =>
  cheminSparkline(historique.value.map((h) => h.cpu), LARGEUR, HAUTEUR),
)
const cheminRam = computed(() =>
  cheminSparkline(historique.value.map((h) => h.ram), LARGEUR, HAUTEUR),
)

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
          <!-- `preserveAspectRatio="none"` étire le repère à la largeur
               disponible ; `vector-effect` empêche l'épaisseur du trait
               d'être étirée avec lui. -->
          <svg
            data-system-history
            :viewBox="`0 0 ${LARGEUR} ${HAUTEUR}`"
            preserveAspectRatio="none"
            class="h-24 w-full"
            role="img"
            :aria-label="t('system_history')"
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
          </svg>
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
      <CardContent class="grid gap-2 text-sm sm:grid-cols-2">
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
