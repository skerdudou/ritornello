<script setup lang="ts">
import { api, Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { formaterDuree, INTERNE, type Donnees, type Envoyer, type T } from './donnees'

const props = defineProps<{
  donnees: Donnees
  t: T
  envoyer: Envoyer
  fige: boolean
  /**
   * Le cœur joue-t-il cette source, d'après son flux poussé.
   *
   * Consulté **en plus** de `donnees.playing`, et les deux ensemble décident s'il
   * faut demander l'arrêt. Chacun couvre une faiblesse de l'autre : le drapeau du
   * plugin pouvait rester à faux après un démarrage où mpv passe brièvement
   * inactif avant de charger, et cette vue-ci est aveugle si `EventSource` est
   * indisponible. Aucun des deux ne peut être un faux positif pour une *autre*
   * source, donc les réunir ne risque pas de couper la radio.
   */
  estSourceActive: boolean
}>()

/**
 * Au-delà de ce nombre de pistes, la liste est paginée.
 *
 * Une liste constituée depuis un partage se compte en milliers de lignes ; en
 * rendre autant de nœuds d'un coup fige l'onglet plusieurs secondes sur le
 * navigateur d'un Raspberry Pi. La pagination a été préférée à une
 * virtualisation par défilement parce qu'elle reste exacte sans mesurer la
 * hauteur des lignes — et qu'un `Ctrl+F` du navigateur trouve ce qui est
 * affiché, au lieu de ne rien trouver dans une fenêtre virtuelle.
 */
const SEUIL_PAGINATION = 200
const TAILLE_PAGE = 100

const nom = ref('')
const destination = ref(INTERNE)
const aCharger = ref('')
const page = ref(0)

const pistes = computed(() => props.donnees.playlist)
const paginee = computed(() => pistes.value.length > SEUIL_PAGINATION)
const pages = computed(() => Math.max(1, Math.ceil(pistes.value.length / TAILLE_PAGE)))

// La page qui contient la piste en cours : arriver sur la page 1 d'une liste de
// trois mille titres alors que le lecteur en est au 1 800e n'aide personne.
watch(
  () => props.donnees.index,
  (i) => {
    if (!paginee.value) return
    page.value = Math.min(pages.value - 1, Math.max(0, Math.floor(i / TAILLE_PAGE)))
  },
  { immediate: true },
)

// Une suppression peut vider la dernière page : sans ce recalage, le volet
// afficherait une fenêtre vide au lieu de la fin de la liste.
watch(pages, (n) => {
  if (page.value > n - 1) page.value = n - 1
})

const decalage = computed(() => (paginee.value ? page.value * TAILLE_PAGE : 0))
const fenetre = computed(() =>
  paginee.value
    ? pistes.value.slice(decalage.value, decalage.value + TAILLE_PAGE)
    : pistes.value,
)

/** Destinations d'enregistrement : le stockage interne, puis les racines inscriptibles. */
const destinations = computed(() => [
  INTERNE,
  ...props.donnees.roots.filter((r) => r.writable).map((r) => r.name),
])

const enregistrees = computed(() => props.donnees.saved)

// Le choix est repéré par son **rang** dans la liste rendue par le plugin, et
// non par une clé composée du nom et de l'emplacement : ces deux-là forment
// bien l'identité d'une liste enregistrée, mais aucun séparateur ne peut les
// joindre sans ambiguïté — un nom de liste contient des espaces, un nom de
// racine des tirets. Le rang évite d'inventer une grammaire de plus, et il est
// recalé dès que le plugin rend une autre liste.
watch(
  enregistrees,
  (liste) => {
    if (Number(aCharger.value) >= liste.length) aCharger.value = '0'
  },
  { immediate: true },
)

function deplacer(depuis: number, vers: number): void {
  if (vers < 0 || vers >= pistes.value.length) return
  void props.envoyer({ op: 'move', from: depuis, to: vers })
}

/**
 * Rang **absolu** de la piste en cours de glissement, ou `null`.
 *
 * Absolu et non relatif à la page : c'est l'index que le plugin attend, et une
 * liste paginée les fait diverger dès la deuxième page.
 */
const glisse = ref<number | null>(null)

/**
 * Dépose la piste glissée à la place de celle survolée.
 *
 * Le glisser-déposer ne couvre que les lignes **visibles** : au-delà de deux
 * cents pistes la liste est paginée, et on ne peut pas glisser vers une page
 * qu'on ne voit pas. Les boutons haut/bas, eux, franchissent les pages — ils
 * restent donc là, et pas seulement pour le clavier.
 */
function deposer(vers: number): void {
  if (glisse.value === null || glisse.value === vers) {
    glisse.value = null
    return
  }
  deplacer(glisse.value, vers)
  glisse.value = null
}

async function retirer(i: number): Promise<void> {
  // Retirer la piste qu'on écoute arrête la lecture, comme vider la liste :
  // continuer à jouer un fichier qui n'y est plus serait la pire des réponses.
  // La comparaison se fait sur l'index **affiché**, celui de la surbrillance que
  // l'utilisateur voit ; `playing`, lui, est relu après coup pour ne pas dépendre
  // d'un état de page périmé.
  const cettePiste = props.donnees.index === i
  const etat = await props.envoyer({ op: 'remove', index: i })
  if (!etat) return
  if (cettePiste && (etat.playing || props.estSourceActive)) await api.post('/api/command', { cmd: 'Stop' })
}

async function vider(): Promise<void> {
  // Vider pendant la lecture laissait la musique continuer sur une liste
  // désormais vide : le plugin ne peut rien demander à mpv — les notifications
  // du SDK sont sans action — donc c'est la page qui demande l'arrêt au cœur, par
  // la même voie que la télécommande. Un geste de l'utilisateur, pas une
  // initiative du plugin.
  //
  // **Seulement si c'est bien cette source qui joue** : sans cette condition, on
  // couperait la radio en vidant une liste de fichiers à l'arrêt.
  // L'état lu est celui **rendu par le vidage**, pas celui qu'affichait la page
  // avant. C'est une fragilité mesurée : `donnees` peut être périmé — la page ne
  // sonde pas en continu — et un `playing` faussement à faux faisait taire la
  // demande d'arrêt sans que rien ne le signale. Le vidage ne touche pas à
  // `playing`, donc sa relecture dit encore la vérité sur ce qui joue.
  const etat = await props.envoyer({ op: 'clear' })
  if (!etat) return
  if (etat.playing || props.estSourceActive) await api.post('/api/command', { cmd: 'Stop' })
}

function enregistrer(): void {
  const n = nom.value.trim()
  if (!n) return
  void props.envoyer({ op: 'save_playlist', name: n, where: destination.value })
}

function charger(): void {
  const choix = enregistrees.value[Number(aCharger.value)]
  if (!choix) return
  void props.envoyer({ op: 'load_playlist', name: choix.name, where: choix.where })
}
</script>

<template>
  <!-- Aucun titre ici : l'onglet qui ouvre ce volet porte deja le meme mot, et
       le repeter juste en dessous ne disait rien de plus. Le volet n'y perd
       pas son nom accessible — `TabsContent` porte un `aria-labelledby` vers
       son declencheur, c'est-a-dire vers ce libelle-la. -->
  <section class="space-y-3" data-volet-liste>
    <p v-if="!pistes.length" class="text-sm text-muted-foreground" data-empty-playlist>
      {{ t('empty_playlist') }}
    </p>

    <template v-else>
      <p v-if="paginee" class="flex items-center gap-2 text-sm text-muted-foreground">
        <Button
          variant="ghost"
          size="sm"
          data-page-prev
          :disabled="page === 0"
          @click="page -= 1"
        >
          ‹
        </Button>
        <span data-page-label>
          {{
            t('page_range', {
              from: decalage + 1,
              to: decalage + fenetre.length,
              total: pistes.length,
            })
          }}
        </span>
        <Button
          variant="ghost"
          size="sm"
          data-page-next
          :disabled="page >= pages - 1"
          @click="page += 1"
        >
          ›
        </Button>
      </p>

      <table class="w-full text-sm">
        <thead class="text-muted-foreground">
          <tr>
            <th class="w-12 text-left font-normal">{{ t('col_num') }}</th>
            <th class="text-left font-normal">{{ t('col_track') }}</th>
            <th class="w-20 text-left font-normal">{{ t('col_duration') }}</th>
            <th class="w-28" />
          </tr>
        </thead>
        <tbody>
          <!-- Lignes déplaçables, comme la grille des stations du plugin radio.
               `dragover.prevent` est indispensable : sans lui le navigateur
               refuse le dépôt. Le rang envoyé au plugin est **absolu**
               (`decalage + i`) et non celui de la page. -->
          <tr
            v-for="(p, i) in fenetre"
            :key="`${decalage + i}:${p.path}`"
            data-track-row
            class="border-t border-border"
            :class="[
              decalage + i === donnees.index ? 'bg-muted/50' : '',
              glisse === decalage + i ? 'opacity-50' : '',
            ]"
            :draggable="!fige"
            @dragstart="glisse = decalage + i"
            @dragover.prevent
            @drop.prevent="deposer(decalage + i)"
            @dragend="glisse = null"
          >
            <!-- `data-track-num` porte le **seul** numéro, et non la cellule :
                 la poignée de glissement y vit aussi, et un test qui lirait le
                 texte de la cellule y trouverait le glyphe. -->
            <td class="whitespace-nowrap tabular-nums text-muted-foreground">
              <span class="cursor-grab select-none pr-1" :title="t('reorder_hint')" data-drag-handle>
                ⠿
              </span>
              <span data-track-num>{{ decalage + i + 1 }}</span>
            </td>
            <td class="py-1 pr-2">
              <span data-track-name>{{ p.name }}</span>
              <!-- Une piste introuvable est **marquée, jamais masquée** : une
                   liste qui rétrécit toute seule est un défaut qu'on met des
                   mois à attribuer, alors qu'un partage démonté se diagnostique
                   en une seconde quand les pistes restent là, signalées. -->
              <span
                v-if="p.missing === true"
                data-track-missing
                :title="p.path"
                class="ml-2 rounded border border-destructive px-1 text-xs text-destructive"
              >
                {{ t('missing_badge') }}
              </span>
              <!-- `null` : le montage ne répondait pas, on ne sait donc pas. Un
                   badge distinct et discret, en gris et non en rouge — dire
                   « introuvable » ici accuserait le fichier d'une panne qui est
                   celle du partage. La bannière au-dessus en donne la cause. -->
              <span
                v-else-if="p.missing === null"
                data-track-unknown
                :title="p.path"
                class="ml-2 rounded border border-muted-foreground px-1 text-xs text-muted-foreground"
              >
                {{ t('missing_unknown') }}
              </span>
            </td>
            <td class="tabular-nums text-muted-foreground">{{ formaterDuree(p.duration_s) }}</td>
            <td class="whitespace-nowrap">
              <Button
                variant="ghost"
                size="icon"
                data-track-up
                :aria-label="t('btn_move_up')"
                :disabled="fige || decalage + i === 0"
                @click="deplacer(decalage + i, decalage + i - 1)"
              >
                ▲
              </Button>
              <Button
                variant="ghost"
                size="icon"
                data-track-down
                :aria-label="t('btn_move_down')"
                :disabled="fige || decalage + i === pistes.length - 1"
                @click="deplacer(decalage + i, decalage + i + 1)"
              >
                ▼
              </Button>
              <Button
                variant="ghost"
                size="icon"
                data-track-remove
                :aria-label="t('btn_remove_track')"
                :disabled="fige"
                @click="retirer(decalage + i)"
              >
                ✕
              </Button>
            </td>
          </tr>
        </tbody>
      </table>

      <Button variant="outline" data-clear :disabled="fige" @click="vider">
        {{ t('btn_clear') }}
      </Button>
    </template>

    <!-- Ce que le chargement d'un m3u n'a pas su retrouver. Sans cet encart, la
         liste chargée serait simplement plus courte que le fichier, sans que
         rien ne le dise. -->
    <div
      v-if="donnees.unresolved.length"
      data-unresolved
      class="space-y-1 rounded-md border border-border p-2"
    >
      <p class="text-sm font-medium">
        {{ t('unresolved_title', { count: donnees.unresolved.length }) }}
      </p>
      <ul class="text-xs text-muted-foreground">
        <li v-for="u in donnees.unresolved" :key="u" data-unresolved-row>{{ u }}</li>
      </ul>
    </div>

    <div class="flex flex-wrap items-end gap-2">
      <Input
        v-model="nom"
        data-playlist-name
        class="w-48"
        :placeholder="t('ph_playlist_name')"
      />
      <select
        v-model="destination"
        data-playlist-where
        class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
        :aria-label="t('dest_label')"
      >
        <!-- Seules les racines **inscriptibles** sont proposées : un partage
             monté en lecture seule refuserait l'écriture côté plugin, et
             l'offrir ici ne produirait qu'un refus. -->
        <option v-for="d in destinations" :key="d" :value="d">
          {{ d === INTERNE ? t('dest_internal') : d }}
        </option>
      </select>
      <Button data-save-playlist :disabled="fige" @click="enregistrer">
        {{ t('btn_save_playlist') }}
      </Button>
    </div>

    <div class="flex flex-wrap items-end gap-2">
      <p v-if="!enregistrees.length" class="text-sm text-muted-foreground" data-no-saved>
        {{ t('no_saved') }}
      </p>
      <template v-else>
        <select
          v-model="aCharger"
          data-saved-pick
          class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
          :aria-label="t('load_playlist_label')"
        >
          <!-- L'emplacement est affiché avec le nom : deux listes « Jazz »
               peuvent coexister, l'une interne, l'autre sur le partage. -->
          <option v-for="(s, i) in enregistrees" :key="`${s.where}/${s.name}`" :value="String(i)">
            {{ s.name }} — {{ s.where === INTERNE ? t('dest_internal') : s.where }}
          </option>
        </select>
        <Button variant="secondary" data-load-playlist :disabled="fige" @click="charger">
          {{ t('btn_load_playlist') }}
        </Button>
      </template>
    </div>
  </section>
</template>
