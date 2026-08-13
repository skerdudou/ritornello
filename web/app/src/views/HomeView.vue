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

// Même valeur que la constante OVERLAY du cœur (2 s) : la fenêtre web et le
// décalage +10 de la télécommande retombent au même rythme.
const FENETRE_MS = 2000
const fenetre = ref(0)
let minuterieFenetre: ReturnType<typeof setTimeout> | undefined

// Compte déclaré par la source (null = source muette sur le sujet : grille
// 1-9 historique, pour ne jamais désarmer la télécommande).
const compte = computed(() => etat.value?.preset_count ?? null)

// Numéros de la fenêtre courante, seulement ceux qui existent. Fenêtre 0 :
// 1-9 (les touches nues de la télécommande) ; fenêtre k : 10k à 10k+9 (le
// 0 de la télécommande donne 10k).
const presets = computed(() => {
  const c = compte.value
  if (c === null) return Array.from({ length: 9 }, (_, i) => i + 1)
  const debut = fenetre.value === 0 ? 1 : fenetre.value * 10
  const fin = Math.min(fenetre.value * 10 + 9, c)
  return debut > fin ? [] : Array.from({ length: fin - debut + 1 }, (_, i) => debut + i)
})

const plus10Visible = computed(() => (compte.value ?? 0) > 9)

function armerRetour() {
  clearTimeout(minuterieFenetre)
  minuterieFenetre = setTimeout(() => { fenetre.value = 0 }, FENETRE_MS)
}

function decaler() {
  // Même règle que le cœur : la dernière fenêtre utile commence au plus
  // grand multiple de 10 encore atteignable ((compte / 10) * 10).
  const c = compte.value ?? 0
  fenetre.value = (fenetre.value + 1) * 10 > c ? 0 : fenetre.value + 1
  armerRetour()
}

function choisir(n: number) {
  // Le web envoie toujours le numéro absolu : Plus10 ne voyage jamais
  // depuis la SPA, la fenêtre est un état purement local.
  clearTimeout(minuterieFenetre)
  fenetre.value = 0
  send({ cmd: 'Select', arg: n })
}

// Un changement de compte (autre source, disque éjecté) invalide la fenêtre.
watch(compte, () => { fenetre.value = 0 })
onUnmounted(() => clearTimeout(minuterieFenetre))

// Timings du volume maintenu, servis par le cœur (modifiables sur la page
// config). Les défauts couvrent le temps du GET et son éventuel échec.
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 1000,
  volume_repeat_interval_ms: 500,
  start_in_standby: false,
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
          :class="['grid grid-cols-3 gap-2',
                    presets.length + (plus10Visible ? 1 : 0) > 9 ? 'sm:grid-cols-10' : 'sm:grid-cols-9']"
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
          <Button v-if="plus10Visible" data-preset-plus10 variant="secondary" @click="decaler">+10</Button>
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
