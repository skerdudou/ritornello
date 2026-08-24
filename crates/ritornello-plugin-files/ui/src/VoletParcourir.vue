<script setup lang="ts">
import { Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { tronquerDebut, type Donnees, type Entree, type Envoyer, type T } from './donnees'

/**
 * Le navigateur de fichiers d'une source déclarée.
 *
 * Un seul niveau à l'écran, et non un arbre qu'on déplie : sur une
 * bibliothèque réelle, l'arbre déplié devenait plus haut que la page et le
 * geste utile — descendre — se perdait dans les rangées des niveaux
 * précédents. Même forme que l'assistant de déclaration (`ChoixDossier`), à
 * ceci près qu'ici les fichiers sont montrés, et pas seulement les dossiers.
 */
const props = defineProps<{ donnees: Donnees; t: T; envoyer: Envoyer; fige: boolean }>()

/** Chemin du niveau supérieur d'une racine : la chaîne vide, comme côté plugin. */
const SOMMET = ''

const racine = ref('')
/** Dossier ouvert, relatif à la racine. */
const chemin = ref(SOMMET)
/**
 * Contenu du dossier ouvert.
 *
 * Mémorisé ici plutôt que lu directement dans `donnees.browse` : le plugin
 * range parcours **et** recherche au même endroit, donc une recherche viderait
 * la liste sous les yeux de l'utilisateur. `null` tant que rien n'a abouti —
 * ce qui n'est pas la même chose qu'un dossier vide.
 */
const entrees = ref<Entree[] | null>(null)
const query = ref('')
const resultats = ref<Entree[] | null>(null)
const tronque = ref(false)
/** La recherche a été interrompue avant d'avoir tout vu, distinct de `tronque`. */
const abandon = ref(false)

function estOuverte(nom: string): boolean {
  return props.donnees.roots.some((r) => r.name === nom)
}

/** Change de racine ou de dossier : ce qui était affiché ne parle plus du bon. */
function reinitialiser(): void {
  chemin.value = SOMMET
  entrees.value = null
  resultats.value = null
  tronque.value = false
  abandon.value = false
  query.value = ''
}

watch(
  // Un nom de racine ne peut contenir ni espace ni virgule (`champ_sur`, côté
  // plugin) : les joindre par un espace donne bien une empreinte injective.
  () => props.donnees.roots.map((r) => r.name).join(' '),
  () => {
    // La racine choisie a pu disparaître d'un enregistrement à l'autre : sans
    // ce recalage, le volet continuerait d'adresser ses `browse` à un nom que
    // le plugin ne connaît plus, et n'afficherait que des refus.
    if (estOuverte(racine.value)) return
    racine.value = props.donnees.roots[0]?.name ?? ''
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

async function charger(cible: string): Promise<void> {
  if (!racine.value) return
  const etat = await props.envoyer({ op: 'browse', root: racine.value, path: cible })
  // Refus : on ne mémorise rien. Mémoriser un niveau vide le ferait passer
  // pour un dossier vide, et l'utilisateur n'aurait aucun moyen de réessayer
  // sans recharger la page.
  if (!etat) return
  const nav = etat.browse
  // On n'accepte que la réponse à la demande qu'on vient de faire : parcours et
  // recherche se rangent au même endroit côté plugin, et une réponse en retard
  // viendrait remplir le mauvais niveau. `query` vide est ce qui distingue un
  // parcours d'une recherche portant sur le même dossier.
  if (nav.root !== racine.value || nav.path !== cible || nav.query !== '') return
  // Les résultats affichés appartiennent au dossier où la recherche a eu
  // lieu : en changer signifie changer de contexte. Sans cet effacement,
  // `search_scope` — un `computed` sur le dossier ouvert — se met à jour
  // seul, et la légende annonce le nouveau dossier au-dessus de résultats
  // qui viennent de l'ancien. Comparé au chemin **accepté**, et non déclenché
  // à chaque appel : `charger` est aussi invoqué au premier affichage et par
  // le recalage de racine, où il n'y a encore rien à effacer — et effacer sans
  // condition y annulerait une saisie en cours de frappe sans rapport.
  if (cible !== chemin.value) {
    resultats.value = null
    tronque.value = false
    abandon.value = false
    query.value = ''
  }
  chemin.value = cible
  entrees.value = nav.entrees
}

function descendre(nom: string): void {
  void charger(chemin.value ? `${chemin.value}/${nom}` : nom)
}

function remonter(): void {
  if (!chemin.value) return
  void charger(chemin.value.replace(/\/?[^/]+$/, ''))
}

/**
 * Adresse du dossier ouvert, nom de la racine compris.
 *
 * Le chemin du plugin est relatif à la racine : affiché seul, il ne dit pas
 * dans laquelle on se trouve dès que plusieurs sources sont déclarées.
 */
const cheminAffiche = computed(() => [racine.value, chemin.value].filter(Boolean).join('/'))
/** Tronqué **par le début** : sur un chemin, l'information utile est la fin. */
const cheminCourt = computed(() => tronquerDebut(cheminAffiche.value))

async function chercher(): Promise<void> {
  const q = query.value.trim()
  if (!q) {
    // Les trois vont ensemble : sans eux, une recherche tronquée ou
    // abandonnée laisserait ces drapeaux à vrai derrière un `resultats` nul —
    // inerte aujourd'hui (le bloc entier est masqué par `v-if="resultats"`),
    // mais c'est la paire d'états que la boucle de correction de la tâche 6
    // s'est employée à garder cohérente partout ailleurs.
    resultats.value = null
    tronque.value = false
    abandon.value = false
    return
  }
  const cible = chemin.value
  const etat = await props.envoyer({ op: 'search', root: racine.value, path: cible, query: q })
  if (!etat) return
  const nav = etat.browse
  if (nav.root !== racine.value || nav.path !== cible || nav.query !== q) return
  resultats.value = nav.resultats
  // Le plugin plafonne la recherche : sans ce drapeau, une liste tronquée
  // passerait pour complète et l'utilisateur conclurait que son fichier n'est
  // pas là.
  tronque.value = nav.tronque
  // Cause distincte : un parcours interrompu avant d'avoir tout vu n'est pas
  // la même chose qu'un motif trop large. Les confondre faisait afficher
  // « Aucun résultat » pour une recherche qui avait simplement renoncé avant
  // d'arriver jusqu'au fichier cherché.
  abandon.value = nav.abandon
}

function ajouterDossier(cible: string): void {
  // Récursif et **asynchrone** côté plugin : la réponse n'attend pas la fin du
  // balayage, c'est le sondage de la page qui en montre l'avancement.
  void props.envoyer({ op: 'add_dir', root: racine.value, path: cible })
}

function ajouterFichier(cible: string): void {
  void props.envoyer({ op: 'add_file', root: racine.value, path: cible })
}

/**
 * Charge un m3u trouvé en parcourant : il **remplace** la liste en cours.
 *
 * Distinct de la liste déroulante des listes *enregistrées* du volet Liste :
 * celle-ci va chercher un nom dans un magasin, tandis qu'ici on désigne un
 * fichier par son chemin, là où il se trouve sur la source.
 */
function chargerListe(cible: string): void {
  void props.envoyer({ op: 'load_m3u', root: racine.value, path: cible })
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
      </div>

      <!-- `min-w-0` partout où du texte long descend : la largeur minimale d'un
           enfant de flex vaut par défaut celle de son contenu, et un chemin long
           pousserait la rangée hors du cadre. C'est aussi ce qui rend `truncate`
           opérant. -->
      <div class="flex min-w-0 items-center gap-2 text-sm">
        <Button
          variant="ghost"
          size="sm"
          class="shrink-0"
          data-browse-up
          :disabled="fige || !chemin"
          @click="remonter"
        >
          ↑ {{ t('btn_up') }}
        </Button>
        <span
          class="min-w-0 flex-1 truncate text-muted-foreground"
          data-browse-path
          :title="cheminAffiche"
        >
          {{ cheminCourt }}
        </span>
        <!-- Absent au sommet : ajouter la source entière vit sur la ligne de la
             source, dans le volet Sources. Deux boutons pour le même effet
             faisaient chercher une différence qui n'existait pas. -->
        <Button
          v-if="chemin"
          variant="secondary"
          size="sm"
          data-add-current
          :disabled="fige"
          @click="ajouterDossier(chemin)"
        >
          {{ t('btn_add_current_folder') }}
        </Button>
      </div>

      <!-- La recherche vit **au-dessus** du listing : elle porte sur le dossier
           ouvert, et la ligne `data-search-scope` le nomme juste en dessous, ce
           qui suffit à dire son périmètre. Sous une liste devenue aussi longue
           que le dossier, il fallait la chercher en défilant. -->
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="query"
          data-search-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="chercher"
        />
        <Button data-search :disabled="fige" @click="chercher">{{ t('btn_search') }}</Button>
      </div>
      <p class="text-xs text-muted-foreground" data-search-scope>
        {{ t('search_scope', { path: cheminAffiche }) }}
      </p>

      <div v-if="resultats" class="space-y-1" data-search-results>
        <!-- Réservé au parcours **complet** : un parcours interrompu avant
             d'avoir tout vu ne dit rien sur la présence du fichier, et
             l'annoncer comme « Aucun résultat » affirmerait le contraire. -->
        <p
          v-if="!resultats.length && !abandon"
          class="text-sm text-muted-foreground"
          data-no-results
        >
          {{ t('no_results') }}
        </p>
        <!-- Le plafond du plugin est silencieux dans la liste : sans cette
             phrase, une recherche tronquée passerait pour complète et
             l'utilisateur conclurait que son fichier n'est pas là. -->
        <p v-if="tronque" class="text-sm text-muted-foreground" data-search-truncated>
          {{ t('search_truncated', { count: resultats.length }) }}
        </p>
        <!-- Cause distincte de `tronque` : ici la marche a renoncé avant
             d'avoir tout parcouru, elle n'a pas trouvé plus que ce qu'elle
             rapporte. Le conseil est donc différent : descendre dans un
             sous-dossier plutôt que préciser le motif. -->
        <p v-if="abandon" class="text-sm text-muted-foreground" data-search-gave-up>
          {{ t('search_gave_up') }}
        </p>
        <!-- Une recherche ne rapporte que des fichiers : `scan::search` filtre
             sur l'audio, et `normaliserBrowse` pose `dir: false` en dur pour ses
             résultats. Le ternaire qui distinguait un dossier ici avait donc une
             branche prouvablement morte, et la clé n'a pas à porter un type qui
             ne varie pas. -->
        <div
          v-for="e in resultats"
          :key="e.path"
          class="flex min-w-0 items-center gap-2 text-sm"
          data-search-row
        >
          <!-- Le chemin complet, pas seulement le nom : une recherche rapporte
               des homonymes de dossiers différents, et rien d'autre ne permet
               de les distinguer. -->
          <span class="min-w-0 flex-1 truncate">{{ e.path }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            :disabled="fige"
            @click="ajouterFichier(e.path)"
          >
            {{ t('btn_add_to_playlist') }}
          </Button>
        </div>
      </div>

      <!-- Aucune hauteur bornée ici : la liste défile **avec** la page. Un
           cadre à barre propre imbriquait deux défilements, et la molette
           s'arrêtait au bord de la liste au lieu de continuer la page. Rien
           n'est repoussé hors de l'écran puisque la recherche est au-dessus. -->
      <ul class="min-w-0 space-y-1 text-sm" data-browse-list>
        <li
          v-for="e in entrees ?? []"
          :key="`${e.dir ? 'd' : 'f'}:${e.path}`"
          data-browse-row
          class="flex min-w-0 items-center gap-2"
        >
          <template v-if="e.dir">
            <button
              type="button"
              data-browse-dir
              class="min-w-0 flex-1 truncate rounded px-2 py-1 text-left hover:bg-accent"
              :disabled="fige"
              :title="e.name"
              @click="descendre(e.name)"
            >
              <span aria-hidden="true" class="mr-1">📁</span
              ><span data-browse-name>{{ e.name }}</span>
            </button>
            <Button
              variant="secondary"
              size="sm"
              data-add-dir
              :disabled="fige"
              @click="ajouterDossier(e.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
          <!-- Une liste de lecture porte une action **différente** : elle
               remplace la liste en cours au lieu de s'y ajouter. Les confondre
               ferait ajouter un fichier texte que mpv tenterait de jouer. -->
          <template v-else-if="e.playlist">
            <span class="min-w-0 flex-1 truncate px-2">
              <span aria-hidden="true" class="mr-1">☰</span
              ><span data-browse-name>{{ e.name }}</span>
            </span>
            <Button
              variant="secondary"
              size="sm"
              data-load-m3u
              :disabled="fige"
              @click="chargerListe(e.path)"
            >
              {{ t('btn_load_m3u') }}
            </Button>
          </template>
          <template v-else>
            <span class="min-w-0 flex-1 truncate px-2" data-browse-name>{{ e.name }}</span>
            <Button
              variant="ghost"
              size="sm"
              data-add-file
              :disabled="fige"
              @click="ajouterFichier(e.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
        </li>
        <!-- `entrees` non nul, donc un niveau a bien été rapporté : un dossier
             réellement vide, et non un parcours qui n'a pas abouti. -->
        <li
          v-if="entrees && !entrees.length"
          class="px-2 text-muted-foreground"
          data-browse-empty
        >
          {{ t('empty_folder') }}
        </li>
      </ul>
    </template>
  </section>
</template>
