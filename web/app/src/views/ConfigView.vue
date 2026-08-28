<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle, Input,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { languageName } from '../composables/languages'
import { useCatalog } from '../composables/useCatalog'
import { usePlugins } from '../composables/usePlugins'
import type { AudioPayload, LocalePayload, SettingsPayload } from '../types'

const { t, reload } = useCatalog()
// L'état des plugins vient du module, step d'un `ref` local : la navigation du
// haut lit le **même** objet, donc une bascule faite ici met son menu à jour
// sans rechargement. Voir `usePlugins`.
const { state: status, refresh: rafraichirGreffons } = usePlugins()
const audio = ref<AudioPayload>({ devices: [], current: null })
const locale = ref<LocalePayload>({ locales: [], current: null })
const device = ref('')
const lang = ref('')
const audioUnavailable = ref(false)
const settings = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  startup_power: 'on',
  date_format: 'day_month_year',
  clock_24h: true,
  overlay_ms: 5000,
  tens_window_ms: 5000,
  seek_step_s: 10,
  cover_cache_entries: 20,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_max_bytes_ko: 512,
  cover_max_pixels_mpx: 16,
})

/**
 * Plafond d'une pochette **reseau** en memoire, en mebioctets.
 *
 * C'est `cover::PLAFOND_RESEAU` cote coeur : un telechargement est coupe la,
 * quoi que dise le plafond de la source. Recopie ici parce que la page ne le
 * recoit step — et une divergence ne rendrait que l'estimation legerement
 * fausse, jamais le reglage incorrect.
 */
const NETWORK_CAP_MIO = 2

/**
 * Ce qu'une entree du cache peut couter au maximum.
 *
 * Le plus petit des deux plafonds : au-dessous de 2 Mio, c'est le plafond de la
 * source qui mord en premier — et il est reglable juste en dessous, donc les
 * deux fields se repondent.
 */
const capPerCover = computed(() =>
  Math.min(NETWORK_CAP_MIO, Number(settings.value.cover_source_max_mio) || NETWORK_CAP_MIO),
)

/**
 * L'estimation haute, en mebioctets : toutes les entries pleines de pochettes
 * **reseau** au plafond.
 *
 * Le pire cas absolu, et il est tres au-dessus du reel : une pochette locale ne
 * garde qu'un chemin, et une pochette de 500 px pese une centaine de
 * kibioctets. C'est justement ce qu'on veut afficher a cote d'un champ qu'on
 * augmente — le majorant, step la moyenne.
 */
const ramMaxCache = computed(
  () => (Number(settings.value.cover_cache_entries) || 0) * capPerCover.value,
)

/**
 * Valeur de vue pour « Par défaut (système) » : jamais envoyée telle quelle
 * (« Changer » la traduit en `device: null`), et impossible à confondre avec
 * un nom de PCM ALSA.
 */
const SYSTEM_DEFAULT = '__system_default__'

// La sélection courante peut nommer un périphérique disparu (carte
// débranchée) : on la garde visible en fin de list plutôt que de laisser
// le déclencheur vide.
const devices = computed(() => {
  const list = [...audio.value.devices]
  const courant = audio.value.current
  if (courant && !list.some((d) => d.name === courant)) {
    list.push({ name: courant, description: '' })
  }
  return list
})

async function loadAll() {
  // Necessaire ici, step redondant : c'est ce qui recharge le catalogue apres
  // un changement de langue reussi (voir `changeLanguage` plus bas), a la
  // place de l'ancien `location.reload()`.
  await reload()
  // Relit l'état des plugins **et** arme la surveillance de la fenêtre « figé »
  // qu'un rallumage vient d'ouvrir : le cœur remplace la ligne dès que le
  // greffon s'annonce, quelques secondes plus tard, et sans cette relecture la
  // ligne restait sur « figé » jusqu'au prochain F5.
  await rafraichirGreffons()
  audioUnavailable.value = false
  audio.value = await api.get<AudioPayload>('/api/audio-output').catch(() => {
    audioUnavailable.value = true
    return audio.value
  })
  locale.value = await api.get<LocalePayload>('/api/locale').catch(() => locale.value)
  settings.value = await api.get<SettingsPayload>('/api/settings').catch(() => settings.value)
  // `current: null` = aucun choix enregistré : c'est l'entrée « Par défaut
  // (système) » qui le porte — plus de repli sur le premier périphérique
  // (c'était `null`, le PCM qui jette le son, en tête de `aplay -L`).
  device.value = audio.value.current ?? SYSTEM_DEFAULT
  lang.value = locale.value.current ?? 'en'
}

onMounted(loadAll)

interface PluginRow {
  name: string
  kinds: string
  connected: boolean
  stalled: boolean
  starting: boolean
  disabled: boolean
  busy: boolean
  admin: boolean
}

/** Accumulateur intermediaire : les genres bruts, avant qu'on decide ce qui
 * doit rester dans `kinds`. Un tableau plutot qu'une chaine construite au fil
 * de l'eau, pour que ce choix ne depende step de l'order d'arrivee. */
interface PluginAccordion {
  name: string
  kindsRecus: string[]
  connected: boolean
  stalled: boolean
  starting: boolean
  disabled: boolean
  busy: boolean
  admin: boolean
}

/**
 * Une ligne par greffon, ses genres joints. Le tableau montrait un couple
 * (nom, genre) par ligne ; la bascule porte sur le nom, et trois interrupteurs
 * qui font tous la même chose ne veulent rien dire.
 *
 * Un greffon n'est « connecté » que si **tous** ses genres le sont : une
 * moitié injoignable est un problème, et l'agrégat ne doit step la cacher.
 */
const plugins = computed<PluginRow[]>(() => {
  const parNom = new Map<string, PluginAccordion>()
  for (const p of status.value.plugins) {
    const acc = parNom.get(p.name)
    if (!acc) {
      parNom.set(p.name, {
        name: p.name,
        kindsRecus: [p.kind],
        connected: p.connected,
        stalled: !!p.stalled,
        starting: !!p.starting,
        disabled: !!p.disabled,
        busy: !!p.busy,
        admin: p.admin,
      })
      continue
    }
    acc.kindsRecus.push(p.kind)
    acc.connected = acc.connected && p.connected
    acc.stalled = acc.stalled || !!p.stalled
    acc.starting = acc.starting || !!p.starting
    acc.disabled = acc.disabled || !!p.disabled
    acc.busy = acc.busy || !!p.busy
    acc.admin = acc.admin || p.admin
  }
  return [...parNom.values()].map((acc) => {
    // « unknown » n'est jamais affiché à côté d'un vrai genre : on ne le
    // garde que quand c'est la seule information reçue pour ce nom. Ça tient
    // par construction, sur l'ensemble complet des genres reçus — step en
    // regardant seulement ce que l'accumulateur contenait à un instant donné,
    // ce qui dépendrait de l'order d'arrivée des lignes.
    const reels = acc.kindsRecus.filter((k) => k !== 'unknown')
    const kinds = (reels.length > 0 ? reels : acc.kindsRecus).join(', ')
    return {
      name: acc.name,
      kinds,
      connected: acc.connected,
      stalled: acc.stalled,
      starting: acc.starting,
      disabled: acc.disabled,
      busy: acc.busy,
      admin: acc.admin,
    }
  })
})

// Noms des plugins dont la bascule est en vol : désactiver l'unique source
// peut coûter jusqu'à 15 s (stop + Deactivate + Activate, chacun capé à 5 s)
// quand l'entrante ou la sortante ne répond step — justement le cas
// d'école qui pousse à désactiver un greffon (un `files` coincé sur un
// partage mort). Sans ce marqueur, l'interrupteur restait cliquable et la
// ligne semblait inerte pendant toute cette fenêtre.
const inProgress = ref<Set<string>>(new Set())

async function togglePlugin(ligne: PluginRow) {
  if (inProgress.value.has(ligne.name)) return
  inProgress.value.add(ligne.name)
  try {
    const actif = ligne.disabled
    const err = await api.put(`/api/plugins/${encodeURIComponent(ligne.name)}/enabled`, {
      enabled: actif,
    })
    if (err) {
      toast.error(err)
    } else {
      toast.success(t.value(actif ? 'plugin_enabled' : 'plugin_disabled', { name: ligne.name }))
    }
    // Rechargement dans les deux cas : un refus a pu laisser l'état d'avant, et
    // un succès change les lignes de plusieurs genres à la fois.
    await loadAll()
  } finally {
    inProgress.value.delete(ligne.name)
  }
}

async function changeOutput() {
  const err = await api.put('/api/audio-output', {
    device: device.value === SYSTEM_DEFAULT ? null : device.value,
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

async function saveSettings() {
  const err = await api.put('/api/settings', {
    ...settings.value,
    volume_repeat_initial_ms: Number(settings.value.volume_repeat_initial_ms),
    volume_repeat_interval_ms: Number(settings.value.volume_repeat_interval_ms),
    overlay_ms: Number(settings.value.overlay_ms),
    tens_window_ms: Number(settings.value.tens_window_ms),
    seek_step_s: Number(settings.value.seek_step_s),
    // Les quatre réglages du rendu sont envoyés **même quand l'interrupteur est
    // décoché**, et c'est délibéré : l'IHM les grise sans les vider, donc
    // recocher l'interrupteur retrouve les valeurs qu'on y avait posées. Les
    // omettre les ferait retomber sur les défauts du cœur (la structure est
    // `serde(default)`), c'est-à-dire perdre en silence un réglage visible à
    // l'écran.
    cover_source_max_mio: Number(settings.value.cover_source_max_mio),
    cover_max_edge_px: Number(settings.value.cover_max_edge_px),
    cover_jpeg_quality: Number(settings.value.cover_jpeg_quality),
    cover_max_bytes_ko: Number(settings.value.cover_max_bytes_ko),
    cover_max_pixels_mpx: Number(settings.value.cover_max_pixels_mpx),
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

// Le changement de langue recharge les catalogues au lieu de reload la
// page entiere comme le faisait l'ancienne IHM.
async function changeLanguage() {
  const err = await api.put('/api/locale', { locale: lang.value })
  if (err) {
    toast.error(err)
    return
  }
  await loadAll()
}

/**
 * Le sommaire : une entrée par carte, dans l'order du gabarit. C'est une
 * donnée (comme REMOTE_ROWS pour la télécommande) : la vue la parcourt pour
 * le nav ET pour l'observation du défilement.
 */
const SECTIONS = [
  { id: 'plugins', key: 'plugins_title' },
  { id: 'audio', key: 'audio_output' },
  { id: 'language', key: 'language' },
  { id: 'startup', key: 'startup_title' },
  { id: 'clock', key: 'clock_title' },
  { id: 'volume-hold', key: 'volume_hold_title' },
  { id: 'overlays', key: 'overlays_title' },
  { id: 'seek', key: 'seek_card_title' },
  { id: 'covers', key: 'cover_card_title' },
] as const

const active = ref<string>(SECTIONS[0].id)
// Visibilité par section, tenue à jour par l'observateur : la section active
// est la première visible dans l'order du sommaire (step la dernière entrée
// reçue, qui dépend de l'order d'arrivée des callbacks).
const visible = new Set<string>()
let observe: IntersectionObserver | null = null

onMounted(() => {
  observe = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) visible.add(e.target.id)
        else visible.delete(e.target.id)
      }
      const premiere = SECTIONS.find((s) => visible.has(s.id))
      if (premiere) active.value = premiere.id
    },
    // La bande d'observation est le haut de l'écran : la section « active »
    // est celle qu'on est en train de lire, step celle qui pointe en bas.
    { rootMargin: '0px 0px -60% 0px' },
  )
  for (const s of SECTIONS) {
    const el = document.getElementById(s.id)
    if (el) observe.observe(el)
  }
})
onUnmounted(() => observe?.disconnect())

function goTo(id: string) {
  active.value = id
  document.getElementById(id)?.scrollIntoView({ behavior: 'smooth' })
}
</script>

<template>
  <div class="flex gap-8">
    <div class="min-w-0 flex-1 space-y-4">
      <section id="plugins" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('plugins_title') }}</CardTitle></CardHeader>
          <CardContent>
            <table class="w-full text-sm">
              <thead class="text-muted-foreground">
                <tr>
                  <th class="text-left font-normal">{{ t('col_plugin') }}</th>
                  <th class="text-left font-normal">{{ t('col_kind') }}</th>
                  <th class="text-left font-normal">{{ t('col_state') }}</th>
                  <th class="text-left font-normal">{{ t('col_admin') }}</th>
                  <th class="text-left font-normal">{{ t('col_enabled') }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="p in plugins" :key="p.name" data-plugin-row class="border-t border-border">
                  <td class="py-1" data-plugin-name>{{ p.name }}</td>
                  <td data-plugin-kind>{{ p.kinds }}</td>
                  <td data-plugin-state>
                    <Badge
                      :variant="
                        p.disabled
                          ? 'outline'
                          : p.busy
                            ? 'outline'
                            : p.connected
                              ? 'secondary'
                              : p.starting
                                ? 'secondary'
                                : p.stalled
                                  ? 'outline'
                                  : 'destructive'
                      "
                    >
                      <!-- « Occupé » passe **avant** « connecté » : un greffon
                           occupé est joint, et c'est justement pour ça que
                           « connecté » ne dit rien d'utile. « Démarrage » passe
                           **avant** « figé » : les deux disent que le greffon
                           n'a step parlé, et seul le temps écoulé les distingue.
                           Afficher « figé » pendant un démarrage normal accusait
                           à tort un binaire parfaitement sain. -->
                      {{
                        p.disabled
                          ? t('disabled')
                          : p.busy
                            ? t('busy')
                            : p.connected
                              ? t('connected')
                            : p.starting
                              ? t('starting')
                              : p.stalled
                                ? t('stalled')
                                : t('unavailable')
                      }}
                    </Badge>
                  </td>
                  <td>
                    <RouterLink v-if="p.admin" :to="`/plugins/${p.name}/`" data-admin-link class="underline">
                      {{ t('admin_link') }}
                    </RouterLink>
                    <span v-else>-</span>
                  </td>
                  <td>
                    <!-- Pas de confirmation : l'action est réversible depuis
                         cette même ligne, et la notification dit ce qui s'est
                         passé. -->
                    <Switch
                      data-plugin-toggle
                      :model-value="!p.disabled"
                      :disabled="inProgress.has(p.name)"
                      :aria-label="t('toggle_plugin', { name: p.name })"
                      @click="togglePlugin(p)"
                    />
                  </td>
                </tr>
              </tbody>
            </table>
          </CardContent>
        </Card>
      </section>

      <section id="audio" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('audio_output') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <!-- Le titre de la carte n'est step associé au déclencheur : sans
                 aria-label, le sélecteur n'a aucun nom accessible. -->
            <Select v-model="device">
              <SelectTrigger class="min-w-64" :aria-label="t('audio_output')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem :value="SYSTEM_DEFAULT" data-audio-default>
                  {{ t('audio_default_device') }}
                </SelectItem>
                <!-- Description lisible en principal, nom technique en
                     secondaire — même motif que « Français » affiché / `fr`
                     envoyé pour les languages. -->
                <SelectItem v-for="d in devices" :key="d.name" :value="d.name">
                  <div class="flex flex-col items-start">
                    <span>{{ d.description || d.name }}</span>
                    <span v-if="d.description" class="text-xs text-muted-foreground">{{ d.name }}</span>
                  </div>
                </SelectItem>
              </SelectContent>
            </Select>
            <Button data-audio-change :disabled="audioUnavailable" @click="changeOutput">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="language" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('language') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <Select v-model="lang">
              <SelectTrigger class="min-w-32" :aria-label="t('language')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <!-- Nom de la langue et non son code : « français » se lit, « fr »
                     se devine. Le code reste la valeur envoyée au cœur. -->
                <SelectItem v-for="l in locale.locales" :key="l" :value="l">
                  {{ languageName(l) }}
                </SelectItem>
              </SelectContent>
            </Select>
            <Button data-lang-change @click="changeLanguage">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="startup" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('startup_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <Select v-model="settings.startup_power">
              <SelectTrigger class="min-w-32" data-startup-select :aria-label="t('startup_title')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="on">{{ t('startup_on') }}</SelectItem>
                <SelectItem value="standby">{{ t('startup_standby') }}</SelectItem>
                <SelectItem value="previous">{{ t('startup_previous') }}</SelectItem>
              </SelectContent>
            </Select>
            <Button data-startup-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Date et heure. Deux settings separes, a la demande du proprietaire :
           l'order d'une date et le format 12/24 h ne varient step ensemble d'un
           country a l'autre. Aucun reglage de fuseau — l'afficheur tourne sur
           l'appareil, la page formate dans le fuseau du browser, et un
           troisieme reglage ne pourrait que contredire l'un des deux. -->
      <section id="clock" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('clock_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
            <label class="grid gap-1 text-sm">
              {{ t('clock_date_label') }}
              <Select v-model="settings.date_format">
                <SelectTrigger class="min-w-36" data-date-format-select :aria-label="t('clock_date_label')"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="day_month_year">{{ t('clock_date_dmy') }}</SelectItem>
                  <SelectItem value="year_month_day">{{ t('clock_date_ymd') }}</SelectItem>
                  <SelectItem value="month_day_year">{{ t('clock_date_mdy') }}</SelectItem>
                </SelectContent>
              </Select>
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('clock_hours_label') }}
              <!-- Un booleen rendu par deux choix nommes plutot qu'une case a
                   cocher : « 24 h » n'est step l'absence de « 12 h », et une
                   case intitulee « 24 h » se lirait mal decochee. -->
              <Select :model-value="settings.clock_24h ? '24' : '12'"
                      @update:model-value="(v) => (settings.clock_24h = v === '24')">
                <SelectTrigger class="min-w-36" data-clock-hours-select :aria-label="t('clock_hours_label')"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="24">{{ t('clock_24h') }}</SelectItem>
                  <SelectItem value="12">{{ t('clock_12h') }}</SelectItem>
                </SelectContent>
              </Select>
            </label>
            <Button data-clock-change @click="saveSettings">{{ t('change') }}</Button>
            <p class="w-full text-sm text-muted-foreground">{{ t('clock_hint') }}</p>
          </CardContent>
        </Card>
      </section>

      <section id="volume-hold" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('volume_hold_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
            <label class="grid gap-1 text-sm">
              {{ t('volume_hold_initial') }}
              <Input type="number" min="200" max="5000" step="100" class="w-28" data-hold-initial
                v-model="settings.volume_repeat_initial_ms" />
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('volume_hold_interval') }}
              <Input type="number" min="100" max="2000" step="50" class="w-28" data-hold-interval
                v-model="settings.volume_repeat_interval_ms" />
            </label>
            <Button data-hold-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="overlays" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('overlays_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
            <label class="grid gap-1 text-sm">
              {{ t('overlay_ms_label') }}
              <Input type="number" min="1000" max="15000" step="500" class="w-28" data-overlay-ms
                v-model="settings.overlay_ms" />
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('tens_window_ms_label') }}
              <Input type="number" min="1000" max="15000" step="500" class="w-28" data-tens-window-ms
                v-model="settings.tens_window_ms" />
            </label>
            <Button data-overlays-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="seek" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('seek_card_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-end gap-4">
            <label class="grid gap-1 text-sm">
              {{ t('seek_step_label') }}
              <Input type="number" min="1" max="120" class="w-28" data-seek-step-s
                v-model="settings.seek_step_s" />
            </label>
            <Button data-seek-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Pochettes. Une seule carte, deux étages qu'il ne faut step confondre,
           et la mise en page porte cette distinction : le plafond de la source
           vient **en premier** et n'est jamais grisé, parce qu'il s'applique
           quoi que dise l'interrupteur — c'est la seule garde qui subsiste
           quand le réencodage est décoché. L'interrupteur vient ensuite, et
           grise les quatre réglages qui ne décrivent que la vignette.

           Grisés, step vidés : les valeurs restent lisibles et repartent dans le
           PUT (voir `saveSettings`), donc recocher l'interrupteur
           retrouve ce qu'on avait posé. -->
      <section id="covers" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('cover_card_title') }}</CardTitle></CardHeader>
          <CardContent class="space-y-4">
            <!-- Hors de l'encart grise du reencodage, comme le plafond de la
                 source : cette borne s'applique quoi qu'il arrive. -->
            <label class="grid gap-1 text-sm">
              {{ t('cover_cache_entries_label') }}
              <Input type="number" min="2" max="100" step="1" class="w-28" data-cover-cache-entries
                v-model="settings.cover_cache_entries" />
              <span class="max-w-md text-xs text-muted-foreground">{{ t('cover_cache_entries_help') }}</span>
              <span class="max-w-md text-xs text-muted-foreground" data-cover-cache-ram>
                {{ t('cover_cache_entries_ram', { size: ramMaxCache, cap: capPerCover }) }}
              </span>
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('cover_source_max_label') }}
              <Input type="number" min="1" max="20" class="w-28" data-cover-source-max
                v-model="settings.cover_source_max_mio" />
              <span class="text-xs text-muted-foreground">{{ t('cover_source_max_help') }}</span>
            </label>

            <div class="border-t border-border pt-4">
              <label class="flex items-start gap-3 text-sm">
                <Switch
                  data-cover-rendition
                  :model-value="settings.cover_rendition"
                  @update:model-value="(v: boolean) => (settings.cover_rendition = v)"
                />
                <span class="grid gap-1">
                  {{ t('cover_rendition_label') }}
                  <span class="text-xs text-muted-foreground">{{ t('cover_rendition_help') }}</span>
                </span>
              </label>
            </div>

            <!-- `aria-disabled` en plus du `disabled` de chaque champ : le
                 groupe entier est inactif, et un player d'écran doit pouvoir
                 l'annoncer une fois plutôt que champ par champ. -->
            <div
              data-cover-rendition-group
              :aria-disabled="!settings.cover_rendition"
              :class="['flex flex-wrap items-start gap-4', settings.cover_rendition ? '' : 'opacity-50']"
            >
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_edge_label') }}
                <Input type="number" min="64" max="2048" class="w-28" data-cover-max-edge
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_edge_px" />
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_jpeg_quality_label') }}
                <Input type="number" min="40" max="100" class="w-28" data-cover-jpeg-quality
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_jpeg_quality" />
                <span class="text-xs text-muted-foreground">{{ t('cover_jpeg_quality_help') }}</span>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_bytes_label') }}
                <Input type="number" min="32" max="8192" class="w-28" data-cover-max-bytes
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_bytes_ko" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_bytes_help') }}</span>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_pixels_label') }}
                <Input type="number" min="1" max="64" class="w-28" data-cover-max-pixels
                  :disabled="!settings.cover_rendition"
                  v-model="settings.cover_max_pixels_mpx" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_pixels_help') }}</span>
              </label>
            </div>

            <Button data-cover-change @click="saveSettings">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>
    </div>

    <nav data-toc :aria-label="t('toc_label')" class="sticky top-6 hidden w-40 shrink-0 self-start lg:block">
      <ul class="space-y-1 text-sm">
        <li v-for="s in SECTIONS" :key="s.id">
          <a
            :href="`#${s.id}`"
            data-toc-link
            :aria-current="active === s.id ? 'true' : undefined"
            :class="active === s.id ? 'font-medium text-foreground' : 'text-muted-foreground'"
            @click.prevent="goTo(s.id)"
          >
            {{ t(s.key) }}
          </a>
        </li>
      </ul>
    </nav>
  </div>
</template>
