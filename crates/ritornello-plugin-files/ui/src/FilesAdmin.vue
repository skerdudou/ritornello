<script setup lang="ts">
import { api, createT, type Catalog } from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { normaliserDonnees, type Donnees } from './donnees'
import VoletListe from './VoletListe.vue'
import VoletParcourir from './VoletParcourir.vue'
import VoletRacines from './VoletRacines.vue'

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

function programmerSondage(): void {
  arreterSondage()
  if (!donnees.value?.scan.running) return
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

// Vol unique : le SDK sert les requêtes d'admin strictement en série, et le
// cœur abandonne au bout de 5 s. Deux opérations déclenchées coup sur coup se
// mettraient en file, la seconde dépassant le plafond — le cœur afficherait
// « plugin injoignable » pour une action pourtant légitime.
const enCours = ref(false)

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
  if (chargementEchoue.value || enCours.value) return null
  enCours.value = true
  try {
    const err = await api.put(url('api/data'), charge)
    if (err) {
      message.value = err
      return null
    }
    message.value = ''
    await recharger()
    return donnees.value
  } finally {
    // Dans un `finally` : une exception ne doit pas laisser la page bloquée
    // sur un vol qui n'a plus lieu.
    enCours.value = false
  }
}

const scan = computed(() => donnees.value?.scan ?? { running: false, found: 0, dir: '' })
</script>

<template>
  <div class="space-y-8">
    <!-- Le message est rendu dans un `<pre>` : l'échec d'un montage porte la
         sortie brute de `systemctl`, sur plusieurs lignes. Un `<p>` la
         replierait en un paragraphe illisible, et c'est pourtant la seule
         chose actionnable que l'utilisateur reçoive. -->
    <pre
      v-if="message"
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

    <template v-if="donnees">
      <VoletRacines
        :donnees="donnees"
        :t="t"
        :envoyer="envoyer"
        :fige="chargementEchoue || enCours"
      />
      <VoletParcourir
        :donnees="donnees"
        :t="t"
        :envoyer="envoyer"
        :fige="chargementEchoue || enCours"
      />
      <VoletListe
        :donnees="donnees"
        :t="t"
        :envoyer="envoyer"
        :fige="chargementEchoue || enCours"
      />
    </template>
  </div>
</template>
