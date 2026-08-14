<script setup lang="ts">
import {
  api, Button, Card, CardAction, CardContent, CardHeader, CardTitle, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import PlayerCard from '../components/PlayerCard.vue'
import { useCatalog } from '../composables/useCatalog'
import { usePlayer } from '../composables/usePlayer'
import type { Command, SettingsPayload } from '../types'
import { REMOTE_POWER, REMOTE_ROWS } from './remoteCommands'
import type { RemoteCommand } from './remoteCommands'

const { t } = useCatalog()

// L'unique connexion SSE de la page vit ici : l'encart Lecteur (en prop) et
// la telecommande (touche active) consomment le meme etat, pousse par
// `/api/player` — rien n'est sonde, et l'etat suit la telecommande infrarouge
// comme les autres onglets.
const { etat, ouvre } = usePlayer()
onMounted(ouvre)

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}

const page = ref(0)

// Compte déclaré par la source (null = source muette sur le sujet : grille
// 1-9 historique, pour ne jamais désarmer la télécommande).
const compte = computed(() => etat.value?.preset_count ?? null)

// Numéros de la page courante, seulement ceux qui existent. Page 0 :
// 1-9 (les touches nues de la télécommande) ; page k : 10k à 10k+9 (le
// 0 de la télécommande donne 10k).
const presets = computed(() => {
  const c = compte.value
  if (c === null) return Array.from({ length: 9 }, (_, i) => i + 1)
  const debut = page.value === 0 ? 1 : page.value * 10
  const fin = Math.min(page.value * 10 + 9, c)
  return debut > fin ? [] : Array.from({ length: fin - debut + 1 }, (_, i) => debut + i)
})

const paginationVisible = computed(() => (compte.value ?? 0) > 9)

// Dernière page non vide : le plus grand multiple de 10 encore atteignable
// (même borne que le rebouclage du +10 côté cœur), 0 si tout tient sur 1-9.
const dernierePage = computed(() => {
  const c = compte.value ?? 0
  return c > 9 ? Math.floor(c / 10) : 0
})

// Les flèches vivent sur la ligne du compteur, hors de la grille : `colonnes`
// ne compte plus que les touches numérotées (9 ou 10 selon la page), qui
// est tout ce qu'elle a jamais eu besoin de savoir. Les mettre dans la grille
// porterait le maximum à 11 cellules et retomberait sur le même défaut que le
// +10 y avait révélé (11 cellules, colonnes plafonnées à 10, dernière cellule
// qui tombe à la ligne) — d'où le placement au-dessus. Classes littérales :
// une classe construite par concaténation n'est pas vue par le scanner
// Tailwind et ne serait pas générée.
const colonnes = computed(() => (presets.value.length === 10 ? 'sm:grid-cols-10' : 'sm:grid-cols-9'))

function pagePrecedente() {
  if (page.value > 0) page.value -= 1
}

function pageSuivante() {
  if (page.value < dernierePage.value) page.value += 1
}

function choisir(n: number) {
  // Le web envoie toujours le numéro absolu ; contrairement à l'ancien +10,
  // choisir une présélection ne referme plus la page — on peut vouloir en
  // essayer plusieurs du même groupe.
  send({ cmd: 'Select', arg: n })
}

// Un changement de compte (autre source, disque éjecté) invalide la page :
// c'est le mécanisme qui porte la garantie « changer de source revient à la
// première page », demandée par le propriétaire — pas de minuterie séparée.
watch(compte, () => { page.value = 0 })

// Timings du volume maintenu, servis par le cœur (modifiables sur la page
// config). Les défauts couvrent le temps du GET et son éventuel échec.
// overlay_ms/tens_window_ms ne sont pas utilisés ici (cet encart ne gère
// que le maintien du volume) mais font partie du même objet servi par
// /api/settings, donc du même repli.
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  start_in_standby: false,
  overlay_ms: 5000,
  tens_window_ms: 5000,
})
onMounted(async () => {
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
})

// Appui maintenu sur Volume +/- : un pas immédiat, puis après le délai
// initial un pas par intervalle jusqu'au relâchement. Miroir côté navigateur
// du cadencement que le cœur applique aux répétitions de la télécommande
// infrarouge — les timings sont les mêmes, servis par /api/settings.
let minuteurInitial: number | null = null
let minuteurIntervalle: number | null = null

function estVolume(c: RemoteCommand) {
  return c.cmd.cmd === 'VolumeUp' || c.cmd.cmd === 'VolumeDown'
}

function debutMaintien(cmd: Command) {
  finMaintien()
  send(cmd)
  minuteurInitial = window.setTimeout(() => {
    send(cmd)
    minuteurIntervalle = window.setInterval(() => send(cmd), reglages.value.volume_repeat_interval_ms)
  }, reglages.value.volume_repeat_initial_ms)
}

function finMaintien() {
  if (minuteurInitial !== null) { window.clearTimeout(minuteurInitial); minuteurInitial = null }
  if (minuteurIntervalle !== null) { window.clearInterval(minuteurIntervalle); minuteurIntervalle = null }
}

onUnmounted(finMaintien)

// Un pas par appui : les répétitions du clavier (keydown auto-répété) sont
// ignorées, le maintien cadencé reste l'affaire du pointeur.
function toucheVolume(e: KeyboardEvent, cmd: Command) {
  if (!e.repeat) send(cmd)
}
</script>

<template>
  <div class="space-y-4">
    <PlayerCard :etat="etat" />
    <Card>
      <!-- La veille au coin de la carte : c'est la seule commande qui agisse sur
           l'appareil entier plutot que sur la lecture, et la plus consequente —
           la tenir a l'ecart de la grille evite de l'actionner par megarde.
           `CardAction` est ce qui la place a droite **sur la ligne du titre** :
           l'en-tete est une grille qui ne passe en deux colonnes qu'en presence
           de ce slot. Sans lui, le bouton tombait sous le titre. -->
      <CardHeader>
        <CardTitle>{{ t('remote_title') }}</CardTitle>
        <CardAction>
          <Button variant="outline" size="sm" data-remote-power @click="send(REMOTE_POWER.cmd)">
            {{ t(REMOTE_POWER.key) }}
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent class="space-y-3">
        <!-- La touche correspondant a ce qui joue est mise en evidence
             (variante pleine + aria-current) : la source active la declare
             (preselection radio, piste cd), et elle s'eteint quand plus rien
             ne joue. -->
        <div
          :class="['grid grid-cols-3 gap-2', colonnes]"
        >
          <Button
            v-for="n in presets"
            :key="n"
            :data-preset-button="n"
            :data-preset-active="etat?.preset === n ? 'true' : undefined"
            :aria-current="etat?.preset === n ? 'true' : undefined"
            :variant="etat?.preset === n ? 'default' : 'secondary'"
            @click="choisir(n)"
          >
            {{ n }}
          </Button>
        </div>
        <!-- Ligne du compteur : flèche précédente, compte, flèche suivante.
             Hors de la grille (voir `colonnes`) pour ne pas en repousser le
             nombre de colonnes. Pas de rebouclage ici, à la différence du +10
             de la télécommande physique : celle-ci n'a qu'une touche et aucun
             moyen de revenir en arrière, donc reboucler est son seul moyen de
             tout couvrir ; avec deux flèches, reboucler serait gratuit et
             déroutant, donc on borne : `<` inactive en première page, `>` en
             dernière. -->
        <div v-if="compte !== null" class="flex items-center justify-between gap-2">
          <Button
            v-if="paginationVisible"
            data-preset-prev
            variant="secondary"
            size="sm"
            :disabled="page === 0"
            :aria-label="t('presets_prev_page')"
            @click="pagePrecedente"
          >
            &lt;
          </Button>
          <!-- Combien de touches la source declare. Utile a deux titres : un
               compte au-dela de la page affichee dit qu'il en existe plus
               loin (c'est ce que la flèche suivante va chercher), et un
               compte de 0 explique une grille vide — un cd sans disque — au
               lieu de la laisser enigmatique. Absent quand la source ne
               declare rien : la grille nue 1-9 est alors un repli, pas un
               inventaire, et annoncer « 9 » serait faux. -->
          <p data-preset-count class="text-xs text-muted-foreground">
            {{ t('presets_label') }} : {{ compte }}
          </p>
          <Button
            v-if="paginationVisible"
            data-preset-next
            variant="secondary"
            size="sm"
            :disabled="page === dernierePage"
            :aria-label="t('presets_next_page')"
            @click="pageSuivante"
          >
            &gt;
          </Button>
        </div>
        <!-- Une rangee par groupe : transport, contenu, son, appareil. Le
             groupement est une donnee (`REMOTE_ROWS`), pas une mise en page
             recopiee ici. -->
        <div v-for="(rangee, i) in REMOTE_ROWS" :key="i" class="flex flex-wrap gap-2" data-remote-row>
          <template v-for="c in rangee" :key="c.key">
            <!-- Volume +/- : appui maintenu (pointeur) au lieu d'un clic. Pas
                 de @click : il partirait en double après le pointerup. Le
                 clavier garde un pas par touche via @keydown (les
                 répétitions auto du navigateur sont filtrées par
                 `toucheVolume`, voir le script). touch-none empêche le
                 défilement tactile d'avaler le maintien, @contextmenu.prevent
                 le menu d'appui long mobile. -->
            <Button
              v-if="estVolume(c)"
              :data-remote-hold="c.cmd.cmd"
              variant="outline"
              class="touch-none select-none"
              @pointerdown="debutMaintien(c.cmd)"
              @pointerup="finMaintien"
              @pointercancel="finMaintien"
              @pointerleave="finMaintien"
              @contextmenu.prevent
              @keydown.enter.prevent="toucheVolume($event, c.cmd)"
              @keydown.space.prevent="toucheVolume($event, c.cmd)"
            >
              {{ t(c.key) }}
            </Button>
            <Button v-else variant="outline" @click="send(c.cmd)">{{ t(c.key) }}</Button>
          </template>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
