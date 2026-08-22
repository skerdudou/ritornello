<script setup lang="ts">
import {
  api, Button, createT, Input, type Catalog,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import DialogueApprentissage from './DialogueApprentissage.vue'
import {
  ACTIONS, codesFor, collect, conflits, parseChamp, presetToml, sanitiseDeviceName,
  type BindingTable, type Conflit,
} from './preset-toml'

// `base` fait partie du contrat des IHM de plugin, au meme titre que
// `catalog` : le prefixe **absolu** sous lequel le coeur sert les routes de ce
// plugin (`/plugins/generic-input/`), fourni par le shell.
//
// Auparavant, cette vue appelait `api.get('./api/data')` en relatif — donc
// resolu contre l'URL du navigateur, et non contre quoi que ce soit que le
// contrat garantisse. Sur `/plugins/generic-input` (sans slash final, forme que
// le routeur du shell acceptait aussi), `./api/data` resolvait vers
// `/plugins/api/data`, que le coeur interprete comme le plugin « api » : 404,
// table vide, erreur de chargement et tous les boutons en echec.
//
// Prop **requise**, sans valeur par defaut : le nom sous lequel ce plugin est
// servi vient de `plugins.toml`, donc du deploiement, et non de ce fichier. Un
// defaut `/plugins/generic-input/` serait faux des que l'operateur declare ce
// plugin sous un autre nom, et le serait *silencieusement*. Mieux vaut que le
// shell soit tenu de fournir le prefixe — ce qu'un test de `PluginView`
// verifie.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** URL absolue d'une route de ce plugin, construite depuis `base`. */
function url(chemin: string): string {
  return `${props.base}${chemin}`
}

const SONDAGE_MS = 300
const DELAI_MS = 30_000

interface Data {
  devices: string[]
  bindings: BindingTable
  presets: string[]
  learning: { captured: number | null } | null
}

const data = ref<Data>({ devices: [], bindings: { devices: [] }, presets: [], learning: null })
const device = ref('')
const preset = ref('')
const codes = ref<string[]>(ACTIONS.map(() => ''))
const message = ref('')
// Ligne (index dans `ACTIONS`) dont on apprend la touche, `null` sinon :
// seule source de verite de l'etat « apprentissage en cours », elle pilote
// l'ouverture de la popin. Ce n'est pas une cible d'ecriture : la destination
// du code capture est la fermeture `i` d'`apprendre`.
const ligneApprise = ref<number | null>(null)
/**
 * Secondes restantes avant l'abandon, pour la popin.
 *
 * Calculees ici et non dans la popin : l'echeance vit avec le sondage, et un
 * second minuteur cote popin deriverait du premier -- il afficherait un
 * chiffre que rien ne garantit. Zero vaut « pas d'apprentissage en cours ».
 */
const secondesRestantes = ref(0)
// Case « ajouter aux codes existants » de la popin, remise a faux a chaque
// ouverture : le geste courant reste le remplacement.
const ajouter = ref(false)
/** Libelle traduit de l'action apprise, pour le titre de la popin. */
const libelleActionApprise = computed(() => {
  const i = ligneApprise.value
  const cle = i === null ? undefined : ACTIONS[i]?.key
  return cle ? t.value(cle) : ''
})
let timer: ReturnType<typeof setInterval> | null = null
// Garde synchrone contre la course decrite en revue (round 1) : `timer` n'est
// affecte qu'apres le `await` du PUT `learn`, donc un second declenchement
// (double-clic, ou clic sur une autre ligne) pendant que ce PUT est en vol
// verrait `timer` toujours nul et passerait lui aussi le garde fonde sur
// `timer` seul -- les deux `setInterval` en resultant, le second ecrasant la
// reference au premier qui devient orphelin (jamais `clearInterval`e) et
// peut ecrire un code capture dans la mauvaise action. Ce drapeau est pose
// avant tout `await`, donc effectif immediatement.
let apprentissageEnVol = false

function remplirCodes() {
  codes.value = ACTIONS.map((a) => (device.value ? codesFor(data.value.bindings, device.value, a.cmd) : ''))
}

async function recharger() {
  try {
    data.value = await api.get<Data>(url('api/data'))
    if (!data.value.devices.includes(device.value)) device.value = data.value.devices[0] ?? ''
    // Meme traitement que `device` : un preset selectionne qui disparaitrait
    // de la liste (ex. suppression du fichier livre) laisserait sinon le
    // `Select` pointer sur une valeur sans `SelectItem` correspondant.
    if (!data.value.presets.includes(preset.value)) preset.value = data.value.presets[0] ?? ''
    remplirCodes()
    message.value = device.value ? '' : t.value('no_device')
  } catch (e) {
    message.value = t.value('load_error') + (e as Error).message
  }
}

onMounted(recharger)
onUnmounted(() => stopTimer())

// Changer de peripherique annule l'apprentissage en cours **avant** de
// repeupler la table, comme le faisait l'ancien gestionnaire
// (`$('dev').onchange = async () => { if (timer) await stopLearn(''); … }`).
//
// Sans cette annulation, l'intervalle continue de sonder alors que la session
// d'apprentissage du serveur est encore armee sur le peripherique
// **precedent** ; `remplirCodes()` a entre-temps repeuple la table depuis les
// bindings du **nouveau** peripherique, donc la fermeture ecrit le code
// capture dans la ligne du nouveau peripherique, et « Enregistrer » le
// persiste — une touche attribuee au mauvais peripherique. L'IHM restait en
// outre en etat « appuyez sur une touche » pour un peripherique que personne
// n'apprend.
//
// `arreterApprentissage` appelle `stopTimer()` de facon synchrone avant tout
// `await` : l'intervalle est donc mort avant que le PUT `cancel_learn` ne
// parte, et aucun sondage ne peut s'intercaler pendant l'aller-retour.
//
// `arreterApprentissage` fait un `await fetch` (PUT `cancel_learn`) qui peut
// rejeter (reseau coupe) : sans `try`/`finally`, la rejection non rattrapee
// sauterait `remplirCodes()`, et les codes du peripherique **precedent**
// resteraient affiches sous le nouveau -- exactement la classe de defaut que
// ce watcher vient corriger, dans la branche d'echec reseau.
watch(device, async () => {
  try {
    if (timer) await arreterApprentissage('')
  } catch {
    // Best-effort : une annulation reseau en echec ne doit pas empecher de
    // repeupler la table pour le nouveau peripherique (voir commentaire
    // ci-dessus).
  } finally {
    remplirCodes()
  }
})

function stopTimer() {
  if (timer) clearInterval(timer)
  timer = null
  // Remis a zero avec le minuteur qui l'alimente : sans cela le dernier
  // chiffre affiche resterait fige derriere le voile, et reapparaitrait tel
  // quel a l'ouverture suivante avant le premier tour de sondage.
  secondesRestantes.value = 0
}

async function arreterApprentissage(texte: string) {
  stopTimer()
  // Avant tout `await`, comme `stopTimer()` : la popin se referme des le
  // geste (annulation, capture, changement de peripherique) et non a la fin
  // de l'aller-retour reseau -- qui peut d'ailleurs echouer.
  ligneApprise.value = null
  await api.put(url('api/data'), { op: 'cancel_learn' })
  message.value = texte
}

/**
 * Annulation demandee par la popin — bouton, croix du kit, Échap, clic sur le
 * voile : quatre gestes, un seul chemin, et une fonction nommee plutot qu'un
 * appel asynchrone ecrit dans le gabarit.
 *
 * La promesse est explicitement abandonnee (`void`) et son echec avale. Rien
 * de ce que voit l'utilisateur n'en depend : `arreterApprentissage` arrete le
 * minuteur et referme la popin **avant** tout `await`. Une panne reseau ne
 * rejette d'ailleurs pas ici -- `api.put` la convertit en valeur de retour
 * (voir `web/kit/src/api.ts`, precisement pour qu'aucun appelant n'ait besoin
 * d'un `try`) ; le `catch` est une ceinture pour le jour ou
 * `arreterApprentissage` gagnerait une etape qui leve, la promesse d'un
 * gestionnaire de gabarit n'ayant nulle part ou etre attendue.
 */
function annulerApprentissage() {
  void arreterApprentissage('').catch(() => {})
}

/** Champ mis a jour pour un code capte : ajout en fin de liste ou remplacement. */
function appliquerCode(champ: string, code: number, ajout: boolean): string {
  if (!ajout || !champ.trim()) return String(code)
  // Comparaison sur les codes analyses, pas sur le texte : `' 9 '` porte bien
  // le code 9. Un `9, 9` serait de toute facon refuse a l'enregistrement
  // (`duplicate_code`), et l'utilisateur n'a rien demande de plus.
  if (parseChamp(champ).includes(code)) return champ
  // Le champ est conserve tel qu'il est ecrit, espaces compris : l'utilisateur
  // a tape ce qu'il a tape.
  return `${champ}, ${code}`
}

// Apprentissage : le plugin capture la prochaine touche du peripherique, la
// vue sonde `GetData` jusqu'a la voir arriver. Meme mecanique que l'ancienne
// page — sondage court, annulation explicite — mais 30 s de delai au lieu de
// 10 : le temps de trouver la bonne touche sur une telecommande inconnue.
async function apprendre(i: number) {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  // Renonce silencieusement si un declenchement precedent est deja en vol :
  // le premier va de toute facon aboutir a une session d'apprentissage
  // valide, et laisser le second continuer produirait un deuxieme
  // `setInterval` concurrent (voir le commentaire sur `apprentissageEnVol`).
  if (apprentissageEnVol) return
  apprentissageEnVol = true
  try {
    if (timer) await arreterApprentissage('')
    const err = await api.put(url('api/data'), { op: 'learn', device: device.value })
    if (err) {
      message.value = err
      return
    }
    // Ceinture et bretelles : au cas ou un minuteur aurait ete (re)installe
    // entre le debut de cette fonction et ici, on ne remplace jamais `timer`
    // sans avoir explicitement arrete l'ancien.
    stopTimer()
    // La consigne « appuyez sur une touche » est portee par la popin, qui
    // nomme l'action et le peripherique : rien a ecrire dans le bandeau du
    // bas, que le voile recouvre de toute facon.
    ligneApprise.value = i
    // ... mais il faut l'effacer : sans cela, le « Delai depasse » de la
    // session precedente y traine encore derriere le voile pendant qu'une
    // popin fraiche attend un appui.
    message.value = ''
    ajouter.value = false
    const echeance = Date.now() + DELAI_MS
    // Pose des maintenant, avant le premier tour du minuteur : sans cela la
    // popin s'ouvrirait sur un compte a rebours vide le temps d'un tour.
    secondesRestantes.value = Math.ceil(DELAI_MS / 1000)
    // Garde de recouvrement : sur une machine lente, un GET qui depasse
    // l'intervalle empilerait des requetes dans la file serielle du plugin —
    // le meme risque que celui documente pour la recherche radio.
    let sondeEnVol = false
    timer = setInterval(async () => {
      if (sondeEnVol) return
      sondeEnVol = true
      try {
        if (Date.now() > echeance) {
          await arreterApprentissage(t.value('learn_timeout'))
          return
        }
        // Arrondi au superieur : a 29,4 s restantes on affiche « 30 », jamais
        // un « 0 » trompeur sur la derniere fraction de seconde -- l'abandon,
        // lui, est decide par la comparaison ci-dessus, pas par ce chiffre.
        secondesRestantes.value = Math.ceil((echeance - Date.now()) / 1000)
        let d: Data
        try {
          d = await api.get<Data>(url('api/data'))
        } catch {
          return // une lecture ratee ne doit pas interrompre le sondage
        }
        const c = d.learning?.captured
        if (c !== null && c !== undefined) {
          codes.value[i] = appliquerCode(codes.value[i] ?? '', c, ajouter.value)
          await arreterApprentissage('')
        }
      } finally {
        sondeEnVol = false
      }
    }, SONDAGE_MS)
  } finally {
    apprentissageEnVol = false
  }
}

// Validation a chaud des doubles affectations : recalculee a chaque frappe,
// `codes` etant un `ref` de tableau lie par `v-model` — un code arrive par
// apprentissage y passe donc aussi, `appliquerCode` ecrivant dans ce meme
// tableau.
const conflitsParAction = computed(() => conflits(codes.value))
const aDesConflits = computed(() => conflitsParAction.value.some((c) => c !== null))

/** Phrase affichee sous un champ fautif. */
function texteConflit(c: Conflit): string {
  if (c.autres.length) {
    // Les libelles **traduits** des autres actions, jamais leurs cles i18n.
    return t.value('conflict_code', { code: c.code, action: c.autres.map((k) => t.value(k)).join(', ') })
  }
  return t.value('conflict_dup', { code: c.code })
}

// Pas de garde sur `aDesConflits` ici : le bouton desactive est la seule voie
// d'appel, et redire la regle dans la fonction creerait deux verites a
// maintenir. Le serveur refuserait de toute facon la table entiere
// (`duplicate_code`).
async function enregistrer() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const table = collect(data.value.bindings, device.value, codes.value)
  const err = await api.put(url('api/data'), { op: 'save', bindings: table })
  if (err) {
    message.value = t.value('save_error') + err
    return
  }
  data.value.bindings = table
  message.value = t.value('saved')
}

async function rafraichir() {
  await api.put(url('api/data'), { op: 'rescan' })
  await recharger()
}

async function chargerPreset() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const err = await api.put(url('api/data'), {
    op: 'load_preset',
    device: device.value,
    preset: preset.value,
  })
  if (err) {
    message.value = err
    return
  }
  await recharger()
}

// Lecture d'un fichier en texte via `FileReader` plutot que `Blob.text()` :
// ce dernier n'est pas implemente par jsdom (environnement des tests), alors
// que `FileReader` y fonctionne, comme dans tout navigateur reel.
function lireFichierTexte(fichier: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const lecteur = new FileReader()
    lecteur.onload = () => resolve(String(lecteur.result ?? ''))
    lecteur.onerror = () => reject(lecteur.error ?? new Error('lecture du fichier impossible'))
    lecteur.readAsText(fichier)
  })
}

// Import : le fichier est lu en texte cote navigateur, puis parse et valide
// cote serveur (`import_preset`) — exactement comme `load_preset` mais sans
// passer par /etc/ritornello/input-presets. Rien n'est persiste avant un
// « Enregistrer » explicite.
async function importer(e: Event) {
  const input = e.target as HTMLInputElement
  const fichier = input.files?.[0]
  input.value = '' // permet de reimporter le meme fichier ensuite
  if (!fichier) return
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  try {
    const contenu = await lireFichierTexte(fichier)
    const err = await api.put(url('api/data'), {
      op: 'import_preset',
      device: device.value,
      content: contenu,
    })
    if (err) {
      message.value = err
      return
    }
    await recharger()
  } catch (err) {
    message.value = t.value('load_error') + (err as Error).message
  }
}

function exporter() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const d = data.value.bindings.devices.find((x) => x.name === device.value)
  const blob = new Blob([presetToml(d ? d.bindings : [])], { type: 'application/toml' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `ritornello-bindings-${sanitiseDeviceName(device.value)}.toml`
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center gap-2">
      <!-- Le <label> voisin n'est pas associe au declencheur (pas de for/id a
           travers le composant Select) : l'aria-label donne le nom accessible. -->
      <label class="text-sm text-muted-foreground">{{ t('device_label') }}</label>
      <Select v-model="device">
        <SelectTrigger data-device-select class="min-w-64" :aria-label="t('device_label')"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="d in data.devices" :key="d" :value="d">{{ d }}</SelectItem>
        </SelectContent>
      </Select>
      <Button variant="secondary" data-refresh @click="rafraichir">{{ t('btn_refresh') }}</Button>
    </div>

    <table class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="text-left font-normal">{{ t('col_action') }}</th>
          <th class="text-left font-normal">{{ t('col_code') }}</th>
          <th class="w-24" /><th class="w-24" />
        </tr>
      </thead>
      <tbody>
        <tr v-for="(a, i) in ACTIONS" :key="a.key" data-action-row class="border-t border-border">
          <td class="py-1">{{ t(a.key) }}</td>
          <td class="py-1 pr-2">
            <!-- Aucune classe rouge a ajouter : l'`Input` du kit porte deja
                 `aria-invalid:border-destructive` et l'anneau rouge. Poser
                 l'attribut est tout le signal. -->
            <Input v-model="codes[i]" inputmode="numeric" :aria-invalid="!!conflitsParAction[i]" />
            <p v-if="conflitsParAction[i]" data-conflict class="mt-1 text-xs text-destructive">
              {{ texteConflit(conflitsParAction[i]!) }}
            </p>
          </td>
          <td><Button variant="secondary" size="sm" data-learn @click="apprendre(i)">{{ t('btn_learn') }}</Button></td>
          <td><Button variant="ghost" size="sm" data-clear @click="codes[i] = ''">{{ t('btn_clear') }}</Button></td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <label class="text-sm text-muted-foreground">{{ t('preset_label') }}</label>
      <Select v-model="preset">
        <SelectTrigger class="min-w-40" :aria-label="t('preset_label')"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="p in data.presets" :key="p" :value="p">{{ p }}</SelectItem>
        </SelectContent>
      </Select>
      <Button variant="secondary" @click="chargerPreset">{{ t('btn_load_preset') }}</Button>
      <label class="cursor-pointer rounded-md border border-border px-3 py-2 text-sm">
        {{ t('btn_import') }}
        <input type="file" accept=".toml" data-import class="hidden" @change="importer" />
      </label>
      <Button variant="secondary" @click="exporter">{{ t('btn_export') }}</Button>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <Button data-save :disabled="aDesConflits" @click="enregistrer">{{ t('btn_save') }}</Button>
      <span v-if="aDesConflits" data-save-blocked class="text-sm text-destructive">{{ t('save_conflicts') }}</span>
      <span class="text-sm text-muted-foreground">{{ message }}</span>
    </div>

    <!-- L'annulation vit desormais dans la popin : un bouton laisse dans la
         barre ci-dessus se retrouverait derriere le voile, donc
         inatteignable. -->
    <DialogueApprentissage
      :ouvert="ligneApprise !== null"
      :t="t"
      :action="libelleActionApprise"
      :device="device"
      :secondes="secondesRestantes"
      v-model:ajouter="ajouter"
      @annuler="annulerApprentissage"
    />
  </div>
</template>
