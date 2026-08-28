<script setup lang="ts">
import { api, Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { formatDuration, INTERNAL, type Data, type Send, type T } from './data'

const props = defineProps<{
  data: Data
  t: T
  send: Send
  fige: boolean
  /**
   * Le cœur joue-t-il cette source, d'après son flux poussé.
   *
   * Consulté **en plus** de `data.playing`, et les deux ensemble décident s'il
   * faut demander l'arrêt. Chacun couvre une faiblesse de l'autre : le drapeau du
   * plugin pouvait rester à faux après un démarrage où mpv passe brièvement
   * inactif avant de load, et cette vue-ci est aveugle si `EventSource` est
   * indisponible. Aucun des deux ne peut être un faux positif pour une *autre*
   * source, donc les réunir ne risque pas de couper la radio.
   */
  isActiveSource: boolean
}>()

/**
 * Au-delà de ce number_ de tracks, la liste est paginée.
 *
 * Une liste constituée depuis un partage se compte en milliers de lignes ; en
 * rendre autant de nœuds d'un coup fige l'onglet plusieurs secondes sur le
 * navigateur d'un Raspberry Pi. La pagination a été préférée à une
 * virtualisation par défilement parce qu'elle reste exacte sans mesurer la
 * hauteur des lignes — et qu'un `Ctrl+F` du navigateur trouve ce qui est
 * affiché, au lieu de ne rien trouver dans une fenêtre virtuelle.
 */
const PAGINATION_THRESHOLD = 200
const PAGE_SIZE = 100

const nom = ref('')
const destination = ref(INTERNAL)
const toLoad = ref('')
const page = ref(0)

const tracks = computed(() => props.data.playlist)
const paginated = computed(() => tracks.value.length > PAGINATION_THRESHOLD)
const pages = computed(() => Math.max(1, Math.ceil(tracks.value.length / PAGE_SIZE)))

// La page qui contient la piste en cours : arriver sur la page 1 d'une liste de
// trois mille titres alors que le player en est au 1 800e n'aide personne.
watch(
  () => props.data.index,
  (i) => {
    if (!paginated.value) return
    page.value = Math.min(pages.value - 1, Math.max(0, Math.floor(i / PAGE_SIZE)))
  },
  { immediate: true },
)

// Une suppression peut clear la dernière page : sans ce recalage, le volet
// afficherait une fenêtre vide au lieu de la fin de la liste.
watch(pages, (n) => {
  if (page.value > n - 1) page.value = n - 1
})

const offset = computed(() => (paginated.value ? page.value * PAGE_SIZE : 0))
const window = computed(() =>
  paginated.value
    ? tracks.value.slice(offset.value, offset.value + PAGE_SIZE)
    : tracks.value,
)

/** Destinations d'enregistrement : le stockage interne, puis les racines inscriptibles. */
const destinations = computed(() => [
  INTERNAL,
  ...props.data.roots.filter((r) => r.writable).map((r) => r.name),
])

const saved = computed(() => props.data.saved)

// Le choix est repéré par son **rang** dans la liste rendue par le plugin, et
// non par une clé composée du nom et de l'emplacement : ces deux-là forment
// bien l'identité d'une liste enregistrée, mais aucun séparateur ne peut les
// joindre sans ambiguïté — un nom de liste contient des espaces, un nom de
// root des tirets. Le rang évite d'inventer une grammaire de plus, et il est
// recalé dès que le plugin rend une autre liste.
watch(
  saved,
  (liste) => {
    if (Number(toLoad.value) >= liste.length) toLoad.value = '0'
  },
  { immediate: true },
)

function move(depuis: number, vers: number): void {
  if (vers < 0 || vers >= tracks.value.length) return
  void props.send({ op: 'move', from: depuis, to: vers })
}

/**
 * Rang **absolu** de la piste en cours de glissement, ou `null`.
 *
 * Absolu et non relatif à la page : c'est l'index que le plugin attend, et une
 * liste paginée les fait diverger dès la deuxième page.
 */
const dragging = ref<number | null>(null)

/**
 * Dépose la piste glissée à la place de celle survolée.
 *
 * Le glisser-déposer ne couvre que les lignes **visibles** : au-delà de deux
 * cents tracks la liste est paginée, et on ne peut pas glisser vers une page
 * qu'on ne voit pas. Les boutons haut/bas, eux, franchissent les pages — ils
 * restent donc là, et pas seulement pour le clavier.
 */
function drop(vers: number): void {
  if (dragging.value === null || dragging.value === vers) {
    dragging.value = null
    return
  }
  move(dragging.value, vers)
  dragging.value = null
}

async function remove(i: number): Promise<void> {
  // Retirer la piste qu'on écoute arrête la lecture, comme clear la liste :
  // continuer à jouer un fichier qui n'y est plus serait la pire des réponses.
  // La comparaison se fait sur l'index **affiché**, celui de la surbrillance que
  // l'utilisateur voit ; `playing`, lui, est relu après coup pour ne pas dépendre
  // d'un état de page périmé.
  const cettePiste = props.data.index === i
  const state = await props.send({ op: 'remove', index: i })
  if (!state) return
  if (cettePiste && (state.playing || props.isActiveSource)) await api.post('/api/command', { cmd: 'Stop' })
}

async function clear(): Promise<void> {
  // Vider pendant la lecture laissait la musique continuer sur une liste
  // désormais vide : le plugin ne peut rien demander à mpv — les notifications
  // du SDK sont sans action — donc c'est la page qui demande l'arrêt au cœur, par
  // la même voie que la télécommande. Un geste de l'utilisateur, pas une
  // initiative du plugin.
  //
  // **Seulement si c'est bien cette source qui joue** : sans cette condition, on
  // couperait la radio en vidant une liste de fichiers à l'arrêt.
  // L'état lu est celui **rendu par le vidage**, pas celui qu'affichait la page
  // avant. C'est une fragilité mesurée : `data` peut être périmé — la page ne
  // sonde pas en continu — et un `playing` faussement à faux faisait taire la
  // demande d'arrêt sans que rien ne le signale. Le vidage ne touche pas à
  // `playing`, donc sa relecture dit encore la vérité sur ce qui joue.
  const state = await props.send({ op: 'clear' })
  if (!state) return
  if (state.playing || props.isActiveSource) await api.post('/api/command', { cmd: 'Stop' })
}

function save(): void {
  const n = nom.value.trim()
  if (!n) return
  void props.send({ op: 'save_playlist', name: n, where: destination.value })
}

function load(): void {
  const choix = saved.value[Number(toLoad.value)]
  if (!choix) return
  void props.send({ op: 'load_playlist', name: choix.name, where: choix.where })
}
</script>

<template>
  <!-- Aucun titre ici : l'onglet qui ouvre ce volet porte deja le meme mot, et
       le repeter juste en dessous ne disait rien de plus. Le volet n'y perd
       pas son nom accessible — `TabsContent` porte un `aria-labelledby` vers
       son declencheur, c'est-a-dire vers ce libelle-la. -->
  <section class="space-y-3" data-volet-liste>
    <p v-if="!tracks.length" class="text-sm text-muted-foreground" data-empty-playlist>
      {{ t('empty_playlist') }}
    </p>

    <template v-else>
      <p v-if="paginated" class="flex items-center gap-2 text-sm text-muted-foreground">
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
              from: offset + 1,
              to: offset + window.length,
              total: tracks.length,
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
               (`offset + i`) et non celui de la page. -->
          <tr
            v-for="(p, i) in window"
            :key="`${offset + i}:${p.path}`"
            data-track-row
            class="border-t border-border"
            :class="[
              offset + i === data.index ? 'bg-muted/50' : '',
              dragging === offset + i ? 'opacity-50' : '',
            ]"
            :draggable="!fige"
            @dragstart="dragging = offset + i"
            @dragover.prevent
            @drop.prevent="drop(offset + i)"
            @dragend="dragging = null"
          >
            <!-- `data-track-num` porte le **seul** numéro, et non la cellule :
                 la poignée de glissement y vit aussi, et un test qui lirait le
                 texte de la cellule y trouverait le glyphe. -->
            <td class="whitespace-nowrap tabular-nums text-muted-foreground">
              <span class="cursor-grab select-none pr-1" :title="t('reorder_hint')" data-drag-handle>
                ⠿
              </span>
              <span data-track-num>{{ offset + i + 1 }}</span>
            </td>
            <td class="py-1 pr-2">
              <span data-track-name>{{ p.name }}</span>
              <!-- Une piste introuvable est **marquée, jamais masquée** : une
                   liste qui rétrécit toute seule est un défaut qu'on met des
                   mois à attribuer, alors qu'un partage démonté se diagnostique
                   en une seconde quand les tracks restent là, signalées. -->
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
            <td class="tabular-nums text-muted-foreground">{{ formatDuration(p.duration_s) }}</td>
            <td class="whitespace-nowrap">
              <Button
                variant="ghost"
                size="icon"
                data-track-up
                :aria-label="t('btn_move_up')"
                :disabled="fige || offset + i === 0"
                @click="move(offset + i, offset + i - 1)"
              >
                ▲
              </Button>
              <Button
                variant="ghost"
                size="icon"
                data-track-down
                :aria-label="t('btn_move_down')"
                :disabled="fige || offset + i === tracks.length - 1"
                @click="move(offset + i, offset + i + 1)"
              >
                ▼
              </Button>
              <Button
                variant="ghost"
                size="icon"
                data-track-remove
                :aria-label="t('btn_remove_track')"
                :disabled="fige"
                @click="remove(offset + i)"
              >
                ✕
              </Button>
            </td>
          </tr>
        </tbody>
      </table>

      <Button variant="outline" data-clear :disabled="fige" @click="clear">
        {{ t('btn_clear') }}
      </Button>
    </template>

    <!-- Ce que le chargement d'un m3u n'a pas su retrouver. Sans cet encart, la
         liste chargée serait simplement plus courte que le fichier, sans que
         rien ne le dise. -->
    <div
      v-if="data.unresolved.length"
      data-unresolved
      class="space-y-1 rounded-md border border-border p-2"
    >
      <p class="text-sm font-medium">
        {{ t('unresolved_title', { count: data.unresolved.length }) }}
      </p>
      <ul class="text-xs text-muted-foreground">
        <li v-for="u in data.unresolved" :key="u" data-unresolved-row>{{ u }}</li>
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
          {{ d === INTERNAL ? t('dest_internal') : d }}
        </option>
      </select>
      <Button data-save-playlist :disabled="fige" @click="save">
        {{ t('btn_save_playlist') }}
      </Button>
    </div>

    <div class="flex flex-wrap items-end gap-2">
      <p v-if="!saved.length" class="text-sm text-muted-foreground" data-no-saved>
        {{ t('no_saved') }}
      </p>
      <template v-else>
        <select
          v-model="toLoad"
          data-saved-pick
          class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
          :aria-label="t('load_playlist_label')"
        >
          <!-- L'emplacement est affiché avec le nom : deux listes « Jazz »
               peuvent coexister, l'une interne, l'autre sur le partage. -->
          <option v-for="(s, i) in saved" :key="`${s.where}/${s.name}`" :value="String(i)">
            {{ s.name }} — {{ s.where === INTERNAL ? t('dest_internal') : s.where }}
          </option>
        </select>
        <Button variant="secondary" data-load-playlist :disabled="fige" @click="load">
          {{ t('btn_load_playlist') }}
        </Button>
      </template>
    </div>
  </section>
</template>
