<script setup lang="ts">
import { Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import type { Donnees, Entree, Envoyer, T } from './donnees'

const props = defineProps<{ donnees: Donnees; t: T; envoyer: Envoyer; fige: boolean }>()

/** Chemin du niveau supérieur d'une racine : la chaîne vide, comme côté plugin. */
const SOMMET = ''

const racine = ref('')
const query = ref('')
const resultats = ref<Entree[] | null>(null)
const tronque = ref(false)

/**
 * Niveaux déjà rapportés, indexés par chemin.
 *
 * C'est ce qui rend l'arbre **paresseux** : un `browse` par niveau réellement
 * ouvert, jamais l'arborescence entière. Sur un NAS de plusieurs dizaines de
 * milliers de fichiers, tout demander d'un coup dépasserait de loin le plafond
 * de 5 s du cœur — la page n'afficherait rien du tout.
 */
const niveaux = ref<Record<string, Entree[]>>({})
const ouverts = ref<string[]>([])

function estOuvert(chemin: string): boolean {
  return ouverts.value.includes(chemin)
}

/** Change de racine : les niveaux mémorisés ne parlent que de l'ancienne. */
function reinitialiser(): void {
  niveaux.value = {}
  ouverts.value = []
  resultats.value = null
  tronque.value = false
}

watch(
  // Un nom de racine ne peut contenir ni espace ni virgule (`champ_sur`, côté
  // plugin) : les joindre par un espace donne bien une empreinte injective.
  () => props.donnees.roots.map((r) => r.name).join(' '),
  () => {
    // La racine choisie a pu disparaître d'un enregistrement à l'autre : sans
    // ce recalage, le volet continuerait d'adresser ses `browse` à un nom que
    // le plugin ne connaît plus, et n'afficherait que des refus.
    const noms = props.donnees.roots.map((r) => r.name)
    if (noms.includes(racine.value)) return
    racine.value = noms[0] ?? ''
    reinitialiser()
    if (racine.value) void charger(SOMMET)
  },
  { immediate: true },
)

function changerRacine(nom: string): void {
  if (nom === racine.value) return
  racine.value = nom
  reinitialiser()
  void charger(SOMMET)
}

async function charger(chemin: string): Promise<void> {
  if (!racine.value) return
  const etat = await props.envoyer({ op: 'browse', root: racine.value, path: chemin })
  // Refus : on ne mémorise rien. Mémoriser un niveau vide le ferait passer
  // pour un dossier vide, et l'utilisateur n'aurait aucun moyen de réessayer
  // sans recharger la page.
  if (!etat) return
  // Le plugin range parcours et recherche **au même endroit** : on n'accepte
  // que ce qui répond bien à la demande qu'on vient de faire, sinon une réponse
  // en retard viendrait remplir le mauvais niveau.
  const nav = etat.browse
  if (nav.root !== racine.value || nav.path !== chemin) return
  niveaux.value = { ...niveaux.value, [chemin]: nav.entrees }
}

/**
 * Plie/déplie un dossier. Le niveau n'est demandé qu'à la **première**
 * ouverture : replier puis rouvrir ne coûte aucune requête.
 */
async function basculer(chemin: string): Promise<void> {
  if (estOuvert(chemin)) {
    ouverts.value = ouverts.value.filter((c) => c !== chemin)
    return
  }
  ouverts.value = [...ouverts.value, chemin]
  if (!(chemin in niveaux.value)) await charger(chemin)
}

interface Rangee {
  entree: Entree
  profondeur: number
}

const rangees = computed<Rangee[]>(() => {
  const out: Rangee[] = []
  const descendre = (chemin: string, profondeur: number): void => {
    for (const e of niveaux.value[chemin] ?? []) {
      out.push({ entree: e, profondeur })
      if (e.dir && estOuvert(e.path)) descendre(e.path, profondeur + 1)
    }
  }
  descendre(SOMMET, 0)
  return out
})

const sommetCharge = computed(() => SOMMET in niveaux.value)

async function chercher(): Promise<void> {
  const q = query.value.trim()
  if (!q) {
    resultats.value = null
    return
  }
  const etat = await props.envoyer({ op: 'search', root: racine.value, query: q })
  if (!etat) return
  resultats.value = etat.browse.resultats
  // Le plugin plafonne la recherche : sans ce drapeau, une liste tronquée
  // passerait pour complète et l'utilisateur conclurait que son fichier n'est
  // pas là.
  tronque.value = etat.browse.tronque
}

function ajouterDossier(chemin: string): void {
  // Récursif et **asynchrone** côté plugin : la réponse n'attend pas la fin du
  // balayage, c'est le sondage de la page qui en montre l'avancement.
  void props.envoyer({ op: 'add_dir', root: racine.value, path: chemin })
}

function ajouterFichier(chemin: string): void {
  void props.envoyer({ op: 'add_file', root: racine.value, path: chemin })
}
</script>

<template>
  <section class="space-y-3" data-volet-parcourir>
    <h2 class="font-medium">{{ t('browse_title') }}</h2>

    <p v-if="!donnees.roots.length" class="text-sm text-muted-foreground">
      {{ t('no_sources') }}
    </p>

    <template v-else>
      <div class="flex flex-wrap items-center gap-2">
        <label class="text-sm text-muted-foreground" for="racine-parcourue">
          {{ t('root_label') }}
        </label>
        <select
          id="racine-parcourue"
          data-browse-root
          class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
          :value="racine"
          :disabled="fige"
          @change="changerRacine(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="r in donnees.roots" :key="r.name" :value="r.name">{{ r.name }}</option>
        </select>
        <Input
          v-model="query"
          data-search-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="chercher"
        />
        <Button data-search :disabled="fige" @click="chercher">{{ t('btn_search') }}</Button>
      </div>

      <!-- Ajouter la source entière ne vit plus ici : chaque ligne du volet
           Sources porte son propre « Ajouter à la liste ». Deux boutons pour le
           même geste, à deux endroits, faisaient hésiter sur leur différence —
           il n'y en avait aucune. -->

      <ul class="space-y-1 text-sm" data-tree>
        <li
          v-for="r in rangees"
          :key="`${r.entree.dir ? 'd' : 'f'}:${r.entree.path}`"
          data-tree-row
          class="flex items-center gap-2"
          :style="{ paddingLeft: `${r.profondeur * 1.25}rem` }"
        >
          <template v-if="r.entree.dir">
            <button
              type="button"
              data-tree-toggle
              class="w-5 shrink-0 text-muted-foreground"
              :aria-expanded="estOuvert(r.entree.path)"
              :aria-label="estOuvert(r.entree.path) ? t('btn_collapse') : t('btn_expand')"
              :disabled="fige"
              @click="basculer(r.entree.path)"
            >
              {{ estOuvert(r.entree.path) ? '▾' : '▸' }}
            </button>
            <span class="flex-1 truncate" data-tree-name>{{ r.entree.name }}</span>
            <Button
              variant="secondary"
              size="sm"
              data-add-dir
              :disabled="fige"
              @click="ajouterDossier(r.entree.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
          <template v-else>
            <span class="w-5 shrink-0" />
            <span class="flex-1 truncate" data-tree-name>{{ r.entree.name }}</span>
            <Button
              variant="ghost"
              size="sm"
              data-add-file
              :disabled="fige"
              @click="ajouterFichier(r.entree.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
        </li>
        <li v-if="sommetCharge && !rangees.length" class="text-muted-foreground" data-tree-empty>
          {{ t('empty_folder') }}
        </li>
      </ul>

      <div v-if="resultats" class="space-y-1" data-search-results>
        <p v-if="!resultats.length" class="text-sm text-muted-foreground" data-no-results>
          {{ t('no_results') }}
        </p>
        <!-- Le plafond du plugin est silencieux dans la liste : sans cette
             phrase, une recherche tronquée passerait pour complète et
             l'utilisateur conclurait que son fichier n'est pas là. -->
        <p v-if="tronque" class="text-sm text-muted-foreground" data-search-truncated>
          {{ t('search_truncated', { count: resultats.length }) }}
        </p>
        <div
          v-for="e in resultats"
          :key="`${e.dir ? 'd' : 'f'}:${e.path}`"
          class="flex items-center gap-2 text-sm"
          data-search-row
        >
          <!-- Le chemin complet, pas seulement le nom : une recherche rapporte
               des homonymes de dossiers différents, et rien d'autre ne permet
               de les distinguer. -->
          <span class="flex-1 truncate">{{ e.path }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            :disabled="fige"
            @click="e.dir ? ajouterDossier(e.path) : ajouterFichier(e.path)"
          >
            {{ t('btn_add_to_playlist') }}
          </Button>
        </div>
      </div>
    </template>
  </section>
</template>
