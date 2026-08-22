<script setup lang="ts">
import {
  api,
  createT,
  surLecteur,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  type Catalog,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { normaliserDonnees, type Donnees } from './donnees'
import VoletListe from './VoletListe.vue'
import VoletParcourir from './VoletParcourir.vue'
import VoletSources from './VoletSources.vue'

// `base` fait partie du contrat des IHM de plugin, au même titre que
// `catalog` : le préfixe **absolu** sous lequel le cœur sert les routes de ce
// plugin (`/plugins/files/`), fourni par le shell.
//
// Prop **requise, sans valeur par défaut** : le nom sous lequel ce plugin est
// servi vient de `plugins.toml`, donc du déploiement. Un défaut
// `/plugins/files/` serait faux — silencieusement — dès que l'opérateur déclare
// ce plugin sous un autre nom. Et toute URL se construit depuis elle : un
// `./api/data` relatif se résoudrait contre l'URL du navigateur, donc vers
// `/plugins/api/data` sur `/plugins/files` (sans slash final), que le cœur
// interprète comme un plugin nommé « api » — 404 et page inerte.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

function url(chemin: string): string {
  return `${props.base}${chemin}`
}

/**
 * Période de sondage pendant un balayage récursif.
 *
 * Le protocole d'admin ne pousse **rien** : il n'y a ni canal d'événements ni
 * websocket derrière le socket d'admin, seulement des requêtes-réponses. Le
 * seul moyen de voir avancer un `add_dir` — qui est asynchrone côté plugin —
 * est donc de redemander `api/data`.
 */
const PERIODE_SONDAGE_MS = 1000

const donnees = ref<Donnees | null>(null)
const message = ref('')
// Vrai tant que le **premier** chargement n'a pas abouti. C'est la garde reprise
// de la page radio, et elle protège ici un dégât du même ordre : après un GET en
// échec, `roots` est vide alors que `media-roots.toml` ne l'est pas, et un
// « Enregistrer les racines » enverrait `{op:'save_roots', roots: []}` — qui
// écrase le fichier et fait disparaître les partages déclarés, sans
// confirmation ni retour arrière.
//
// L'échec d'un sondage **ultérieur** ne la lève pas : les données sont alors
// déjà là, elles ne mentent pas, et rendre la page inerte parce qu'un
// rafraîchissement d'une seconde a échoué serait une régression de confort pour
// aucun gain de sûreté.
const chargementEchoue = ref(false)

let minuterie: ReturnType<typeof setTimeout> | null = null

function arreterSondage(): void {
  if (minuterie !== null) {
    clearTimeout(minuterie)
    minuterie = null
  }
}

/**
 * Y a-t-il un travail en cours dont la page attend la fin ?
 *
 * Deux, et il a fallu un parcours de bout en bout pour s'en souvenir : le
 * balayage récursif, **et** la connexion à un partage. Le protocole admin ne
 * pousse rien, donc tout ce qui est asynchrone côté plugin n'arrive à l'écran
 * que par ce sondage. Ne surveiller que le balayage laissait la popin réseau
 * bloquée sur « Connexion… » pour toujours — le plugin avait pourtant répondu,
 * mais plus personne ne le relisait.
 */
function travailEnCours(): boolean {
  return (
    donnees.value?.scan.running === true ||
    donnees.value?.explore.busy === true ||
    // Le relevé des durées : elles arrivent par lots, et sans ce sondage la
    // colonne resterait à « — » jusqu'au prochain geste de l'utilisateur.
    donnees.value?.durations.running === true
  )
}

/**
 * Un assistant est ouvert : c'est lui qui porte les refus, pas la page.
 *
 * Le bandeau de la page vit **derrière** le voile gris de la boîte de dialogue.
 * L'y laisser en double revenait à afficher le refus là où on ne peut pas le
 * lire, au moment précis où il compte.
 */
const popinOuverte = computed(() => donnees.value?.explore.open === true)

function programmerSondage(): void {
  arreterSondage()
  if (!travailEnCours()) return
  minuterie = setTimeout(() => {
    void recharger()
  }, PERIODE_SONDAGE_MS)
}

async function recharger(): Promise<void> {
  try {
    donnees.value = normaliserDonnees(await api.get<unknown>(url('api/data')))
    programmerSondage()
  } catch (e) {
    // Le message de chargement n'écrase pas un refus déjà affiché s'il y en a
    // un : les deux racontent le même incident, et le premier est le plus
    // précis (il vient du catalogue du serveur).
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
    if (donnees.value === null) chargementEchoue.value = true
    arreterSondage()
  }
}

onMounted(recharger)
// Sans cela, la minuterie survit au démontage : le shell change de page, le
// composant est détruit, et un `recharger()` continue de tourner toutes les
// secondes contre un composant mort.
onUnmounted(arreterSondage)

/**
 * Relit l'état quand le lecteur change, pour que la piste surlignée suive.
 *
 * Le surlignage vient d'`index`, que seul `api/data` porte — et le sondage
 * s'arrête dès qu'aucun travail n'est en cours. La piste en cours changeant
 * d'elle-même à chaque fin de morceau, le surlignage restait donc figé sur
 * celle du début.
 *
 * Un flux poussé plutôt qu'un sondage permanent : le cœur annonce déjà chaque
 * changement, et sonder en continu ferait marteler le plugin tant qu'un onglet
 * reste ouvert. Une relecture de plus au moment du changement, et rien entre
 * deux.
 */
let fermerLecteur: (() => void) | null = null

/**
 * Nom sous lequel ce plugin est servi, déduit de `base`.
 *
 * Déduit et non écrit en dur : il vient de `plugins.toml`, donc du déploiement.
 * `base` **est** cette information, il n'y a rien à reconstruire.
 */
const nomPlugin = computed(() => props.base.replace(/^\/plugins\//, '').replace(/\/+$/, ''))

/**
 * Cette source est-elle celle que le cœur joue, d'après le flux poussé.
 *
 * C'est la vérité du **cœur**, et elle ne peut pas dériver — contrairement au
 * drapeau que le plugin tenait, qui pouvait rester à faux après un démarrage où
 * mpv passe brièvement inactif avant de charger le premier fichier. Les deux
 * sont consultés ensemble (voir `VoletListe`), pour couvrir aussi le cas où
 * `EventSource` est indisponible.
 */
const sourceActive = ref<string | null>(null)
const estSourceActive = computed(() => sourceActive.value === nomPlugin.value)

onMounted(() => {
  fermerLecteur = surLecteur((etat) => {
    const s = (etat as { source?: unknown } | null)?.source
    sourceActive.value = typeof s === 'string' ? s : null
    // Pas pendant un envoi : le SDK sert les requêtes en série, et une relecture
    // qui s'y ajouterait ferait dépasser le plafond de 5 s du cœur.
    if (!enCours.value && !chargementEchoue.value) void recharger()
  })
})
onUnmounted(() => fermerLecteur?.())

// Vol unique : le SDK sert les requêtes d'admin strictement en série, et le
// cœur abandonne au bout de 5 s. Deux opérations déclenchées coup sur coup se
// mettraient en file, la seconde dépassant le plafond — le cœur répondrait
// par la phrase traduite de son catalogue (`plugin_timeout`) pour une action
// pourtant légitime.
/** Un envoi est en vol : c'est ce qui interdit le double-envoi. */
const envoiEnCours = ref(false)
/** Une relecture suit un envoi : l'IHM reste grisée, mais un observateur a le
 * droit d'émettre — voir le commentaire d'`envoyer`. */
const rechargement = ref(false)
/** Ce qui grise l'IHM : l'un ou l'autre. */
const enCours = computed(() => envoiEnCours.value || rechargement.value)

/**
 * Envoie une opération, puis relit l'état.
 *
 * Un refus arrive sous la forme `{"error": "<phrase déjà traduite>"}` : la
 * phrase est produite par les catalogues i18n du serveur et affichée **telle
 * quelle**. En particulier, l'échec de `{"op":"mount"}` porte la sortie de
 * `systemctl` : c'est elle qui est actionnable, la reformuler la détruirait.
 */
async function envoyer(charge: Record<string, unknown>): Promise<Donnees | null> {
  // Ceinture et bretelles : la protection ne repose pas sur le seul `disabled`
  // des boutons, qu'un outil de développement ou un futur remaniement du
  // gabarit pourrait contourner — alors que la conséquence (l'écrasement de
  // `media-roots.toml` par une table vide) est irréversible.
  //
  // Le vol ne couvre que **l'envoi**, pas la relecture qui suit. Le rechargement
  // met à jour `donnees`, ce qui déclenche le flush de rendu de Vue, donc les
  // observateurs des volets — dont celui qui charge le premier niveau de
  // l'arbre quand les racines changent. Tant que le vol couvrait aussi la
  // relecture, cet observateur appelait `envoyer` alors que le verrou était
  // encore pris : il recevait `null`, et rien ne le relançait ensuite (le
  // sondage n'est armé que pendant un balayage). Symptôme mesuré au parcours
  // e2e : après avoir enregistré une racine, le volet Parcourir restait
  // désespérément vide.
  if (chargementEchoue.value || envoiEnCours.value) return null
  envoiEnCours.value = true
  let err: string | null
  try {
    err = await api.put(url('api/data'), charge)
  } finally {
    // Dans un `finally` : une exception ne doit pas laisser la page bloquée sur
    // un vol qui n'a plus lieu.
    envoiEnCours.value = false
  }
  if (err) {
    message.value = err
    return null
  }
  message.value = ''
  // `rechargement` remplace le vol pour ce qui est de griser l'IHM : les
  // boutons restent inertes le temps de la relecture, sans pour autant
  // empêcher un observateur d'émettre son propre envoi.
  rechargement.value = true
  try {
    await recharger()
  } finally {
    rechargement.value = false
  }
  return donnees.value
}

const scan = computed(
  () => donnees.value?.scan ?? { running: false, found: 0, dir: '', error: '' },
)
</script>

<template>
  <div class="space-y-8">
    <!-- Le message est rendu dans un `<pre>` : l'échec d'un montage porte la
         sortie brute de `systemctl`, sur plusieurs lignes. Un `<p>` la
         replierait en un paragraphe illisible, et c'est pourtant la seule
         chose actionnable que l'utilisateur reçoive. -->
    <pre
      v-if="message && !popinOuverte"
      data-message
      class="whitespace-pre-wrap rounded-md border border-border bg-muted/40 p-2 font-mono text-sm"
      >{{ message }}</pre
    >

    <!-- Avancement du balayage. Il n'apparaît que pendant un `add_dir`, et
         c'est le sondage — pas une notification du plugin — qui le fait
         avancer. -->
    <p v-if="scan.running" data-scan class="text-sm text-muted-foreground">
      {{ t('scan_progress', { found: scan.found, dir: scan.dir }) }}
    </p>

    <!-- Avancement du relevé des durées. Le dire plutôt que de laisser la
         colonne se remplir toute seule : sur un partage lent, une liste qui
         change sous les yeux sans explication inquiète. -->
    <p
      v-if="donnees?.durations.running"
      data-durations
      class="text-sm text-muted-foreground"
    >
      {{
        t('duration_progress', {
          done: donnees.durations.done,
          total: donnees.durations.total,
        })
      }}
    </p>

    <!-- Incident du **dernier** balayage, déjà traduit par le plugin et affiché
         verbatim. Il survit à la fin du balayage, et c'est le seul endroit où la
         page peut apprendre qu'un ajout a échoué : `add_dir` rend la main bien
         avant la fin de la marche récursive, donc son accusé de réception ne
         dit rien de son issue. -->
    <pre
      v-if="scan.error"
      data-scan-error
      class="whitespace-pre-wrap rounded-md border border-destructive p-2 font-mono text-sm"
      >{{ scan.error }}</pre
    >

    <!-- Trois onglets plutôt que trois volets bout à bout : la page demandait
         un long défilement pour atteindre la déclaration d'une source, geste
         rare, alors que la liste et le navigateur sont les deux écrans qu'on
         ouvre vraiment.

         `force-mount` partout, et ce n'est pas un détail : sans lui les
         panneaux inactifs seraient démontés, si bien que revenir sur
         « Parcourir » après un détour rouvrirait la racine de la source au
         lieu du dossier où l'on se trouvait — et relancerait un `browse` à
         chaque va-et-vient. Les volets restent donc vivants, seul l'affichage
         change. -->
    <Tabs v-if="donnees" default-value="liste">
      <TabsList>
        <!-- `data-onglet` porte la valeur et non seulement le marqueur : le
             parcours de bout en bout doit désigner un onglet sans dépendre de
             son libellé, qui est traduit. -->
        <TabsTrigger value="liste" data-onglet="liste">{{ t('playlist_title') }}</TabsTrigger>
        <TabsTrigger value="parcourir" data-onglet="parcourir">
          {{ t('browse_title') }}
        </TabsTrigger>
        <TabsTrigger value="sources" data-onglet="sources">{{ t('sources_title') }}</TabsTrigger>
      </TabsList>

      <TabsContent value="liste" force-mount>
        <VoletListe
          :donnees="donnees"
          :t="t"
          :envoyer="envoyer"
          :fige="chargementEchoue || enCours"
          :est-source-active="estSourceActive"
        />
      </TabsContent>
      <TabsContent value="parcourir" force-mount>
        <VoletParcourir
          :donnees="donnees"
          :t="t"
          :envoyer="envoyer"
          :fige="chargementEchoue || enCours"
        />
      </TabsContent>
      <TabsContent value="sources" force-mount>
        <VoletSources
          :donnees="donnees"
          :t="t"
          :envoyer="envoyer"
          :fige="chargementEchoue || enCours"
          :message="message"
        />
      </TabsContent>
    </Tabs>
  </div>
</template>
