<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, createT, Input, Label, toast, type Catalog } from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` fait partie du contract des IHM de plugin, au meme titre que
// `catalog` : le prefixe **absolu** sous lequel le coeur sert les routes de ce
// plugin (`/plugins/musicbrainz/`), fourni par le shell. Prop **requise**,
// sans valeur par defaut, pour la meme raison que dans `MpdAdmin.vue` : le nom
// sous lequel ce plugin est servi vient de `plugins.toml`, donc du
// deploiement, et un defaut serait faux — silencieusement — des qu'un
// operateur le declare sous un autre nom.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** URL absolue d'une route de ce plugin, construite depuis `base`. */
function url(chemin: string): string {
  return `${props.base}${chemin}`
}

// --- Le contract get_data / set_data (tache 8), recopie ici tel quel --------
//
// `motif` est une enumeration **etiquetee a l'exterieur** : soit l'objet
// `{ separe: {...} }`, soit la chaine nue `"ne_pas_decouper"`. Ce n'est pas
// un objet avec un champ `type` — le typer comme une union de ces deux formes
// exactes evite de reconstituer une forme qui n'existe pas cote serveur.
interface SplitPattern {
  separe: {
    separateur: string
    artiste_en_premier: boolean
    /** La forme `Artiste - Titre - Album` : le titre est le champ du milieu.
     *
     * Optionnel parce que le champ est additif cote dorsal (`serde(default)`),
     * donc un fichier d'etat ecrit avant lui se relit sans l'avoir. Et la page
     * ne le **produit** jamais : ce motif ne s'obtient que par un sondage,
     * jamais a la main — le jeu ferme de l'edition ne le propose pas. */
    titre_au_milieu?: boolean
  }
}
type Pattern = SplitPattern | 'ne_pas_decouper'
type Origin = 'standard_confirme' | 'deviation_apprise' | 'manuel'

interface Station {
  url: string
  motif: Pattern
  origine: Origin
  // Present et nul quand la station n'a jamais servi (pas absent) : le type
  // porte cette possibilite explicitement plutot que de la traiter en aval
  // comme un champ optionnel qui pourrait aussi manquer.
  dernier_usage: string | null
  titres_decoupes: number
}

interface Data {
  stations: Station[]
}

const data = ref<Data>({ stations: [] })

/**
 * Filtre « exceptions seulement », **actif par defaut** : une station dont le
 * format a ete confirme standard existe bien comme entree (son absence
 * confondrait « jamais sondee » et « verifiee conforme »), mais ce que
 * l'operateur vient chercher ici, ce sont les stations qui devient — ce
 * filtre les isole du bruit des stations qui marchent deja.
 */
const filterExceptions = ref(true)

const shownStations = computed(() =>
  filterExceptions.value
    ? data.value.stations.filter((s) => s.origine !== 'standard_confirme')
    : data.value.stations,
)

// Deux etats de vide distincts, jamais fusionnes : un ecran vide serait sinon
// ambigu entre « tout va bien » et « rien n'a jamais fonctionne ».
const nothingProbed = computed(() => data.value.stations.length === 0)
const filterHidesAll = computed(() => !nothingProbed.value && shownStations.value.length === 0)

async function reload(): Promise<void> {
  try {
    data.value = await api.get<Data>(url('api/data'))
  } catch (e) {
    // Comme `MpdAdmin.vue` : aucune cle de catalogue ne couvre cet echec de
    // chargement, le message brut de la requete est le seul texte
    // disponible.
    toast.error((e as Error).message)
  }
}

onMounted(reload)

// --- Libelles ----------------------------------------------------------

// Des appels litteraux (`t.value('origin_standard')`, etc.), pas une
// indirection par table : `i18nKeysUsed.test.ts` ne collecte que les cles
// passees en clair a `t`/`t.value`, une cle recomposee depuis une variable
// lui echapperait silencieusement.
function originText(o: Origin): string {
  switch (o) {
    case 'standard_confirme':
      return t.value('origin_standard')
    case 'deviation_apprise':
      return t.value('origin_learned')
    case 'manuel':
      return t.value('origin_manual')
  }
}

function patternText(m: Pattern): string {
  if (m === 'ne_pas_decouper') return t.value('pattern_no_split')
  // La forme `Artiste - Titre - Album` porte le meme separateur et le meme
  // order que le standard : sans mention propre, elle s'afficherait comme lui
  // et la page mentirait par omission sur la seule colonne qu'on vient y lire.
  const order = m.separe.titre_au_milieu
    ? t.value('pattern_title_middle')
    : m.separe.artiste_en_premier
      ? t.value('pattern_artist_first')
      : t.value('pattern_title_first')
  return `"${m.separe.separateur}" (${order})`
}

// --- Edition -------------------------------------------------------------
//
// Un jeu ferme, jamais une expression rationnelle : une regex libre ferait
// deboguer des expressions a l'utilisateur, et une mauvaise casserait tous
// les titres de la station. Les seuls choix sont un separateur (une chaine,
// pas un motif), un order (deux valeurs), et « ne pas decouper », qui grise
// les deux precedents.

/** URL de la station en cours d'edition, `null` si aucune. Une seule ligne a
 *  la fois : ouvrir une deuxieme edition referme implicitement la premiere
 *  (voir `openEdit`). */
const rowBeingEdited = ref<string | null>(null)
const edSeparator = ref('')
const edOrder = ref<'artist_first' | 'title_first'>('artist_first')
const edDoNotSplit = ref(false)
/** La forme `Artiste - Titre - Album`, **conservee et non offerte** : aucun
 * champ du formulaire ne la pose, mais l'edition d'une entree qui la porte doit
 * la rejouer a l'identique. Voir `openEdit`. */
const edTitleInMiddle = ref(false)

function openEdit(s: Station): void {
  rowBeingEdited.value = s.url
  if (s.motif === 'ne_pas_decouper') {
    edDoNotSplit.value = true
    edSeparator.value = ''
    edOrder.value = 'artist_first'
  } else {
    edDoNotSplit.value = false
    edSeparator.value = s.motif.separe.separateur
    edOrder.value = s.motif.separe.artiste_en_premier ? 'artist_first' : 'title_first'
    // Conserve, et non offert : le formulaire ne propose pas cette forme — elle
    // ne s'obtient que par un sondage — mais il doit la **rejouer** telle
    // quelle. Sans cette ligne, ouvrir l'edition d'une station en
    // « Artiste - Titre - Album » puis enregistrer sans rien changer degradait
    // son motif, et l'entree devenant manuelle, plus rien ne la reparait.
    edTitleInMiddle.value = s.motif.separe.titre_au_milieu === true
  }
}

function cancelEdit(): void {
  rowBeingEdited.value = null
}

/**
 * Erreur de validation du separateur, ou `null` s'il est valide — recalculee
 * a chaque frappe. Reprend **les memes cles de catalogue** que celles que le
 * dorsal renvoie pour ces deux refus precis (`separator_empty`,
 * `separator_no_space`), pour que le retour immediat cote page dise
 * exactement ce que dirait un refus serveur. Elle ne s'applique pas quand
 * « ne pas decouper » est coche : le separateur est alors hors-jeu.
 */
const separatorError = computed(() => {
  if (edDoNotSplit.value) return null
  // `trim()` et non la seule vacuite : un separateur qui n'est que des espaces
  // passait les deux controles (`' '` commence *et* finit par une espace, la
  // meme) et aurait decoupe sur chaque espace du titre annonce. Meme predicat
  // que le dorsal, qui reste l'autorite.
  if (!edSeparator.value.trim()) return t.value('separator_empty')
  if (!(edSeparator.value.startsWith(' ') && edSeparator.value.endsWith(' '))) {
    return t.value('separator_no_space')
  }
  return null
})

function buildPattern(): Pattern {
  if (edDoNotSplit.value) return 'ne_pas_decouper'
  return {
    separe: {
      separateur: edSeparator.value,
      artiste_en_premier: edOrder.value === 'artist_first',
      titre_au_milieu: edTitleInMiddle.value,
    },
  }
}

/**
 * Poste l'action `pose`. La page valide le separateur pour un retour
 * immediat (`separatorError`), **mais** le dorsal reste l'autorite : cette
 * meme saisie peut encore etre refusee la-bas (fichier d'etat inscriptible,
 * course avec un autre client admin), auquel cas son message — deja une
 * phrase traduite, jamais une cle — est affiche tel quel, sans retraduction.
 */
async function saveEdit(): Promise<void> {
  if (separatorError.value) return
  const stationUrl = rowBeingEdited.value
  if (!stationUrl) return
  const err = await api.put(url('api/data'), { action: 'pose', url: stationUrl, motif: buildPattern() })
  if (err) {
    toast.error(err)
    return
  }
  rowBeingEdited.value = null
  await reload()
}

async function remove(s: Station): Promise<void> {
  const err = await api.put(url('api/data'), { action: 'supprime', url: s.url })
  if (err) {
    toast.error(err)
    return
  }
  // La ligne supprimee pouvait etre en cours d'edition : sans cette garde, le
  // formulaire resterait ouvert sur une station qui n'existe plus.
  if (rowBeingEdited.value === s.url) rowBeingEdited.value = null
  await reload()
}

async function clear(): Promise<void> {
  const err = await api.put(url('api/data'), { action: 'vide' })
  if (err) {
    toast.error(err)
    return
  }
  rowBeingEdited.value = null
  await reload()
}
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t('title') }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p class="text-sm text-muted-foreground">{{ t('intro') }}</p>

      <div class="flex flex-wrap items-center justify-between gap-2">
        <label class="flex items-center gap-2 text-sm">
          <input type="checkbox" data-filtre-exceptions v-model="filterExceptions" />
          {{ t('filter_exceptions_only') }}
        </label>
        <Button variant="secondary" data-clear @click="clear">{{ t('clear_all') }}</Button>
      </div>

      <p v-if="nothingProbed" data-empty class="text-sm text-muted-foreground">{{ t('empty') }}</p>
      <p v-else-if="filterHidesAll" data-empty-filtered class="text-sm text-muted-foreground">
        {{ t('empty_filtered') }}
      </p>

      <!-- Conteneur de defilement propre a la table : l'URL d'un flux est
           longue, et cette page ne doit pas faire defiler la page entiere
           pour l'accommoder. -->
      <div v-if="!nothingProbed && !filterHidesAll" class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead class="text-muted-foreground">
            <tr>
              <th class="text-left font-normal">{{ t('col_station') }}</th>
              <th class="text-left font-normal">{{ t('col_pattern') }}</th>
              <th class="text-left font-normal">{{ t('col_origin') }}</th>
              <th class="text-left font-normal">{{ t('col_last_used') }}</th>
              <th class="text-left font-normal">{{ t('col_split_count') }}</th>
              <th class="text-left font-normal">{{ t('col_actions') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in shownStations" :key="s.url" data-station-ligne class="border-t border-border align-top">
              <!-- `max-w-0` force la colonne a respecter la largeur du
                   `<table>` plutot que de s'etendre a la longueur de l'URL :
                   c'est ce qui permet a `truncate` de s'appliquer. -->
              <td class="max-w-0 truncate py-2 pr-2" :title="s.url">{{ s.url }}</td>

              <td class="py-2 pr-2">
                <template v-if="rowBeingEdited === s.url">
                  <div class="flex flex-col gap-1">
                    <Label class="text-xs font-normal text-muted-foreground">{{ t('field_separator') }}</Label>
                    <Input
                      data-separateur v-model="edSeparator" :disabled="edDoNotSplit"
                      :aria-invalid="!!separatorError"
                    />
                    <Label class="text-xs font-normal text-muted-foreground">{{ t('field_order') }}</Label>
                    <select
                      data-order v-model="edOrder" :disabled="edDoNotSplit"
                      class="rounded-md border border-input bg-transparent px-2 py-1 text-sm disabled:opacity-50"
                    >
                      <option value="artist_first">{{ t('pattern_artist_first') }}</option>
                      <option value="title_first">{{ t('pattern_title_first') }}</option>
                    </select>
                    <label class="flex items-center gap-2">
                      <input type="checkbox" data-ne-pas-decouper v-model="edDoNotSplit" />
                      {{ t('field_no_split') }}
                    </label>
                    <p v-if="separatorError" data-separateur-error class="text-xs text-destructive">
                      {{ separatorError }}
                    </p>
                  </div>
                </template>
                <template v-else>{{ patternText(s.motif) }}</template>
              </td>

              <td class="py-2 pr-2">{{ originText(s.origine) }}</td>
              <td class="py-2 pr-2">{{ s.dernier_usage ?? '—' }}</td>
              <td class="py-2 pr-2">{{ s.titres_decoupes }}</td>

              <td class="py-2">
                <template v-if="rowBeingEdited === s.url">
                  <div class="flex flex-wrap gap-1">
                    <Button size="sm" data-enregistrer-edition :disabled="!!separatorError" @click="saveEdit">
                      {{ t('save') }}
                    </Button>
                    <Button size="sm" variant="secondary" data-annuler-edition @click="cancelEdit">
                      {{ t('cancel') }}
                    </Button>
                  </div>
                </template>
                <template v-else>
                  <div class="flex flex-wrap gap-1">
                    <Button size="sm" variant="secondary" data-editer @click="openEdit(s)">
                      {{ t('edit') }}
                    </Button>
                    <Button size="sm" variant="secondary" data-remove @click="remove(s)">
                      {{ t('delete') }}
                    </Button>
                  </div>
                </template>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </CardContent>
  </Card>
</template>
