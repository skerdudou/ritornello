<script setup lang="ts">
import {
  api, Button, createT, Input, type Catalog,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` fait partie du contrat des IHM de plugin, au meme titre que
// `catalog` : le prefixe **absolu** sous lequel le coeur sert les routes de ce
// plugin (`/plugins/radio/`), fourni par le shell.
//
// Auparavant, cette vue appelait `api.get('./api/data')` en relatif — donc
// resolu contre l'URL du navigateur, et non contre quoi que ce soit que le
// contrat garantisse. Sur `/plugins/radio` (sans slash final, forme que le
// routeur du shell acceptait aussi), `./api/data` resolvait vers
// `/plugins/api/data`, que le coeur interprete comme le plugin « api » : 404,
// table vide, erreur de chargement et tous les boutons en echec.
//
// Prop **requise**, sans valeur par defaut : le nom sous lequel ce plugin est
// servi vient de `plugins.toml`, donc du deploiement, et non de ce fichier. Un
// defaut `/plugins/radio/` serait faux des que l'operateur declare ce plugin
// sous un autre nom, et le serait *silencieusement*. Mieux vaut que le shell
// soit tenu de fournir le prefixe — ce qu'un test de `PluginView` verifie.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** URL absolue d'une route de ce plugin, construite depuis `base`. */
function url(chemin: string): string {
  return `${props.base}${chemin}`
}

// Les chiffres de la telecommande : au-dela, l'ajout est refuse cote IHM
// plutot que de laisser l'enregistrement echouer. `Stations::validate` reste
// l'autorite serveur.
const MAX = 9

// Reka UI refuse une `<SelectItem>` de valeur vide (« doit pouvoir designer
// l'absence de selection »). Le pays « tous » est donc porte par ce jeton
// interne cote IHM uniquement : `chercher()` le traduit en chaine vide avant
// l'envoi, pour rester conforme au contrat `country: ''` du plugin.
const TOUS_PAYS = '__ALL__'

interface Station { name: string; url: string }
interface Trouvee { name: string; url: string; codec: string; bitrate: number; country: string }

const stations = ref<Station[]>([])
const resultats = ref<Trouvee[] | null>(null)
const query = ref('')
const pays = ref('FR')
const message = ref('')
const rechercheEnCours = ref(false)
// Garde-fou repris de l'ancienne page, qui terminait son gestionnaire d'echec
// de chargement par `document.querySelectorAll('button').forEach((b) => {
// b.disabled = true })`. Sa raison d'etre : apres un GET en echec, `stations`
// reste **vide** alors que la table servie par le plugin, elle, ne l'est pas.
// Un « Enregistrer » enverrait `{op:'save', stations: []}`, que
// `Stations::validate` accepte (elle itere sur un vecteur vide) et qui ecrase
// `stations.toml` : toutes les preselections de l'utilisateur disparaissent,
// sans confirmation ni retour arriere.
//
// Ce n'est pas theorique : le plugin sert les requetes d'admin strictement en
// serie, avec un budget annuaire de 4 s contre le plafond de 5 s du coeur,
// donc un chargement concurrent d'une recherche peut faire echouer le GET
// alors qu'un PUT ulterieur reussira. Un redemarrage du plugin entre les deux
// produit le meme effet.
//
// L'etat est **collant** : comme l'ancienne page, il n'y a pas de « reessayer
// » ici, seul un rechargement de la page reprend un etat sain. Mieux vaut une
// page inerte qu'une page qui detruit les donnees qu'elle n'a pas su lire.
const chargementEchoue = ref(false)

async function recharger(): Promise<void> {
  try {
    const data = await api.get<{ stations: Array<Station & { preset: number }>; search?: Trouvee[] }>(
      url('api/data'),
    )
    stations.value = [...data.stations]
      .sort((a, b) => a.preset - b.preset)
      .map(({ name, url }) => ({ name, url }))
    if (data.search?.length) resultats.value = data.search
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
    chargementEchoue.value = true
  }
}

onMounted(recharger)

// Rien n'est persiste avant « Enregistrer » : l'ajout n'agit que sur la table
// en cours d'edition.
function ajouter(s: Station = { name: '', url: '' }): boolean {
  if (stations.value.length >= MAX) {
    message.value = t.value('limit_reached')
    return false
  }
  stations.value.push({ ...s })
  message.value = ''
  return true
}

function supprimer(i: number): void {
  stations.value.splice(i, 1)
}

// Numerotation automatique : la presélection est la **position** de la ligne.
// Consequence assumee : supprimer une ligne renumerote les suivantes.
async function enregistrer(): Promise<void> {
  // Ceinture et bretelles : la protection ne repose pas sur le seul etat
  // visuel du bouton. Un `disabled` peut etre contourne (outils de
  // developpement, extension, futur refactor du template qui oublierait la
  // liaison) alors que la consequence — l'ecrasement de `stations.toml` par
  // une table vide — est irreversible.
  if (chargementEchoue.value) return
  const charge = stations.value.map((s, i) => ({ preset: i + 1, name: s.name, url: s.url }))
  const err = await api.put(url('api/data'), { op: 'save', stations: charge })
  message.value = err ? t.value('save_error') + err : t.value('saved')
}

// Vol unique : le SDK sert les requetes d'admin strictement en serie. Un
// second declenchement pendant qu'une recherche court se mettrait en file
// derriere la premiere et, l'annuaire etant en panne (budget de 4 s cote
// plugin), depasserait le plafond de 5 s du coeur — qui afficherait
// « plugin injoignable ». La garde est partagee par le bouton et la touche
// Entree, et levee dans un `finally` pour se rétablir aussi bien apres une
// erreur qu'apres un succes.
async function chercher(): Promise<void> {
  // Meme garde qu'`enregistrer()` (voir son commentaire) : `:disabled` sur
  // le bouton ne protege pas `@keydown.enter`, qui atteint `chercher()`
  // meme apres un chargement en echec. Sans ce retour anticipe, une
  // recherche reussie ferait `message.value = ''`, effacant le message
  // d'erreur de chargement alors que `chargementEchoue` reste vrai : la
  // page paraitrait saine alors qu'elle est inerte (voir aussi la garde
  // ceinture-et-bretelles au debut d'`enregistrer()`).
  if (chargementEchoue.value) return
  if (rechercheEnCours.value) return
  const q = query.value.trim()
  if (!q) {
    message.value = t.value('empty_query')
    return
  }
  rechercheEnCours.value = true
  message.value = t.value('searching')
  try {
    const country = pays.value === TOUS_PAYS ? '' : pays.value
    const err = await api.put(url('api/data'), { op: 'search', query: q, country })
    if (err) {
      message.value = err
      return
    }
    const data = await api.get<{ search?: Trouvee[] }>(url('api/data'))
    resultats.value = data.search ?? []
    message.value = ''
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
  } finally {
    rechercheEnCours.value = false
  }
}

function libelle(s: Trouvee): string {
  return `${s.name} — ${s.codec} ${s.bitrate} kbps${s.country ? ` (${s.country})` : ''}`
}
</script>

<template>
  <div class="space-y-6">
    <table class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="w-10 text-left font-normal">{{ t('col_num') }}</th>
          <th class="text-left font-normal">{{ t('col_name') }}</th>
          <th class="text-left font-normal">{{ t('col_url') }}</th>
          <th class="w-10" />
        </tr>
      </thead>
      <tbody>
        <tr v-for="(s, i) in stations" :key="i" class="border-t border-border">
          <td data-station-num class="tabular-nums text-muted-foreground">{{ i + 1 }}</td>
          <td class="py-1 pr-2"><Input v-model="s.name" data-station-name /></td>
          <td class="py-1 pr-2"><Input v-model="s.url" data-station-url /></td>
          <td>
            <Button variant="ghost" size="icon" data-station-delete @click="supprimer(i)">✕</Button>
          </td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <!-- Les trois actions sont neutralisees quand le chargement a echoue,
           en miroir de la desactivation globale de l'ancienne page. -->
      <Button variant="secondary" data-add :disabled="chargementEchoue" @click="ajouter()">
        {{ t('btn_add') }}
      </Button>
      <Button data-save :disabled="chargementEchoue" @click="enregistrer">{{ t('btn_save') }}</Button>
      <span class="text-sm text-muted-foreground">{{ message }}</span>
    </div>

    <section class="space-y-2">
      <h2 class="font-medium">{{ t('search_title') }}</h2>
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="query"
          data-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="chercher"
        />
        <Select v-model="pays">
          <SelectTrigger class="w-40"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="FR">{{ t('country_fr') }}</SelectItem>
            <SelectItem value="US">{{ t('country_us') }}</SelectItem>
            <SelectItem :value="TOUS_PAYS">{{ t('country_all') }}</SelectItem>
          </SelectContent>
        </Select>
        <Button data-search :disabled="rechercheEnCours || chargementEchoue" @click="chercher">
          {{ t('btn_search') }}
        </Button>
      </div>
      <ul v-if="resultats" class="space-y-1 text-sm">
        <li v-if="!resultats.length" class="text-muted-foreground">{{ t('no_results') }}</li>
        <li v-for="(s, i) in resultats" :key="i" class="flex items-center gap-2">
          <!-- textContent par interpolation, jamais de v-html : le nom vient
               d'un annuaire public. -->
          <span class="flex-1">{{ libelle(s) }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            @click="ajouter({ name: s.name, url: s.url })"
          >
            {{ t('btn_add_result') }}
          </Button>
        </li>
      </ul>
    </section>
  </div>
</template>
