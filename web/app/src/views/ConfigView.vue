<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle, Input,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { nomLangue } from '../composables/langues'
import { useCatalog } from '../composables/useCatalog'
import { useGreffons } from '../composables/useGreffons'
import type { AudioPayload, LocalePayload, SettingsPayload } from '../types'

const { t, reload } = useCatalog()
// L'état des greffons vient du module, pas d'un `ref` local : la navigation du
// haut lit le **même** objet, donc une bascule faite ici met son menu à jour
// sans rechargement. Voir `useGreffons`.
const { etat: status, rafraichir: rafraichirGreffons } = useGreffons()
const audio = ref<AudioPayload>({ devices: [], current: null })
const locale = ref<LocalePayload>({ locales: [], current: null })
const device = ref('')
const lang = ref('')
const audioIndisponible = ref(false)
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  startup_power: 'on',
  overlay_ms: 5000,
  tens_window_ms: 5000,
  seek_step_s: 10,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_max_bytes_ko: 512,
  cover_max_pixels_mpx: 16,
})

/**
 * Valeur de vue pour « Par défaut (système) » : jamais envoyée telle quelle
 * (« Changer » la traduit en `device: null`), et impossible à confondre avec
 * un nom de PCM ALSA.
 */
const DEFAUT_SYSTEME = '__system_default__'

// La sélection courante peut nommer un périphérique disparu (carte
// débranchée) : on la garde visible en fin de liste plutôt que de laisser
// le déclencheur vide.
const appareils = computed(() => {
  const liste = [...audio.value.devices]
  const courant = audio.value.current
  if (courant && !liste.some((d) => d.name === courant)) {
    liste.push({ name: courant, description: '' })
  }
  return liste
})

async function chargerTout() {
  // Necessaire ici, pas redondant : c'est ce qui recharge le catalogue apres
  // un changement de langue reussi (voir `changerLangue` plus bas), a la
  // place de l'ancien `location.reload()`.
  await reload()
  // Relit l'état des greffons **et** arme la surveillance de la fenêtre « figé »
  // qu'un rallumage vient d'ouvrir : le cœur remplace la ligne dès que le
  // greffon s'annonce, quelques secondes plus tard, et sans cette relecture la
  // ligne restait sur « figé » jusqu'au prochain F5.
  await rafraichirGreffons()
  audioIndisponible.value = false
  audio.value = await api.get<AudioPayload>('/api/audio-output').catch(() => {
    audioIndisponible.value = true
    return audio.value
  })
  locale.value = await api.get<LocalePayload>('/api/locale').catch(() => locale.value)
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
  // `current: null` = aucun choix enregistré : c'est l'entrée « Par défaut
  // (système) » qui le porte — plus de repli sur le premier périphérique
  // (c'était `null`, le PCM qui jette le son, en tête de `aplay -L`).
  device.value = audio.value.current ?? DEFAUT_SYSTEME
  lang.value = locale.value.current ?? 'en'
}

onMounted(chargerTout)

interface LigneGreffon {
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
 * de l'eau, pour que ce choix ne depende pas de l'ordre d'arrivee. */
interface AccGreffon {
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
 * moitié injoignable est un problème, et l'agrégat ne doit pas la cacher.
 */
const greffons = computed<LigneGreffon[]>(() => {
  const parNom = new Map<string, AccGreffon>()
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
    // par construction, sur l'ensemble complet des genres reçus — pas en
    // regardant seulement ce que l'accumulateur contenait à un instant donné,
    // ce qui dépendrait de l'ordre d'arrivée des lignes.
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

// Noms des greffons dont la bascule est en vol : désactiver l'unique source
// peut coûter jusqu'à 15 s (stop + Deactivate + Activate, chacun capé à 5 s)
// quand l'entrante ou la sortante ne répond pas — justement le cas
// d'école qui pousse à désactiver un greffon (un `files` coincé sur un
// partage mort). Sans ce marqueur, l'interrupteur restait cliquable et la
// ligne semblait inerte pendant toute cette fenêtre.
const enCours = ref<Set<string>>(new Set())

async function basculerGreffon(ligne: LigneGreffon) {
  if (enCours.value.has(ligne.name)) return
  enCours.value.add(ligne.name)
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
    await chargerTout()
  } finally {
    enCours.value.delete(ligne.name)
  }
}

async function changerSortie() {
  const err = await api.put('/api/audio-output', {
    device: device.value === DEFAUT_SYSTEME ? null : device.value,
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

async function enregistrerReglages() {
  const err = await api.put('/api/settings', {
    ...reglages.value,
    volume_repeat_initial_ms: Number(reglages.value.volume_repeat_initial_ms),
    volume_repeat_interval_ms: Number(reglages.value.volume_repeat_interval_ms),
    overlay_ms: Number(reglages.value.overlay_ms),
    tens_window_ms: Number(reglages.value.tens_window_ms),
    seek_step_s: Number(reglages.value.seek_step_s),
    // Les quatre réglages du rendu sont envoyés **même quand l'interrupteur est
    // décoché**, et c'est délibéré : l'IHM les grise sans les vider, donc
    // recocher l'interrupteur retrouve les valeurs qu'on y avait posées. Les
    // omettre les ferait retomber sur les défauts du cœur (la structure est
    // `serde(default)`), c'est-à-dire perdre en silence un réglage visible à
    // l'écran.
    cover_source_max_mio: Number(reglages.value.cover_source_max_mio),
    cover_max_edge_px: Number(reglages.value.cover_max_edge_px),
    cover_jpeg_quality: Number(reglages.value.cover_jpeg_quality),
    cover_max_bytes_ko: Number(reglages.value.cover_max_bytes_ko),
    cover_max_pixels_mpx: Number(reglages.value.cover_max_pixels_mpx),
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

// Le changement de langue recharge les catalogues au lieu de recharger la
// page entiere comme le faisait l'ancienne IHM.
async function changerLangue() {
  const err = await api.put('/api/locale', { locale: lang.value })
  if (err) {
    toast.error(err)
    return
  }
  await chargerTout()
}

/**
 * Le sommaire : une entrée par carte, dans l'ordre du gabarit. C'est une
 * donnée (comme REMOTE_ROWS pour la télécommande) : la vue la parcourt pour
 * le nav ET pour l'observation du défilement.
 */
const SECTIONS = [
  { id: 'plugins', key: 'plugins_title' },
  { id: 'audio', key: 'audio_output' },
  { id: 'language', key: 'language' },
  { id: 'startup', key: 'startup_title' },
  { id: 'volume-hold', key: 'volume_hold_title' },
  { id: 'overlays', key: 'overlays_title' },
  { id: 'seek', key: 'seek_card_title' },
  { id: 'covers', key: 'cover_card_title' },
] as const

const active = ref<string>(SECTIONS[0].id)
// Visibilité par section, tenue à jour par l'observateur : la section active
// est la première visible dans l'ordre du sommaire (pas la dernière entrée
// reçue, qui dépend de l'ordre d'arrivée des callbacks).
const visibles = new Set<string>()
let observer: IntersectionObserver | null = null

onMounted(() => {
  observer = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) visibles.add(e.target.id)
        else visibles.delete(e.target.id)
      }
      const premiere = SECTIONS.find((s) => visibles.has(s.id))
      if (premiere) active.value = premiere.id
    },
    // La bande d'observation est le haut de l'écran : la section « active »
    // est celle qu'on est en train de lire, pas celle qui pointe en bas.
    { rootMargin: '0px 0px -60% 0px' },
  )
  for (const s of SECTIONS) {
    const el = document.getElementById(s.id)
    if (el) observer.observe(el)
  }
})
onUnmounted(() => observer?.disconnect())

function aller(id: string) {
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
                <tr v-for="p in greffons" :key="p.name" data-plugin-row class="border-t border-border">
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
                           n'a pas parlé, et seul le temps écoulé les distingue.
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
                      :disabled="enCours.has(p.name)"
                      :aria-label="t('toggle_plugin', { name: p.name })"
                      @click="basculerGreffon(p)"
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
            <!-- Le titre de la carte n'est pas associé au déclencheur : sans
                 aria-label, le sélecteur n'a aucun nom accessible. -->
            <Select v-model="device">
              <SelectTrigger class="min-w-64" :aria-label="t('audio_output')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem :value="DEFAUT_SYSTEME" data-audio-default>
                  {{ t('audio_default_device') }}
                </SelectItem>
                <!-- Description lisible en principal, nom technique en
                     secondaire — même motif que « Français » affiché / `fr`
                     envoyé pour les langues. -->
                <SelectItem v-for="d in appareils" :key="d.name" :value="d.name">
                  <div class="flex flex-col items-start">
                    <span>{{ d.description || d.name }}</span>
                    <span v-if="d.description" class="text-xs text-muted-foreground">{{ d.name }}</span>
                  </div>
                </SelectItem>
              </SelectContent>
            </Select>
            <Button data-audio-change :disabled="audioIndisponible" @click="changerSortie">{{ t('change') }}</Button>
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
                  {{ nomLangue(l) }}
                </SelectItem>
              </SelectContent>
            </Select>
            <Button data-lang-change @click="changerLangue">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <section id="startup" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('startup_title') }}</CardTitle></CardHeader>
          <CardContent class="flex flex-wrap items-center gap-2">
            <Select v-model="reglages.startup_power">
              <SelectTrigger class="min-w-32" data-startup-select :aria-label="t('startup_title')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="on">{{ t('startup_on') }}</SelectItem>
                <SelectItem value="standby">{{ t('startup_standby') }}</SelectItem>
                <SelectItem value="previous">{{ t('startup_previous') }}</SelectItem>
              </SelectContent>
            </Select>
            <Button data-startup-change @click="enregistrerReglages">{{ t('change') }}</Button>
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
                v-model="reglages.volume_repeat_initial_ms" />
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('volume_hold_interval') }}
              <Input type="number" min="100" max="2000" step="50" class="w-28" data-hold-interval
                v-model="reglages.volume_repeat_interval_ms" />
            </label>
            <Button data-hold-change @click="enregistrerReglages">{{ t('change') }}</Button>
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
                v-model="reglages.overlay_ms" />
            </label>
            <label class="grid gap-1 text-sm">
              {{ t('tens_window_ms_label') }}
              <Input type="number" min="1000" max="15000" step="500" class="w-28" data-tens-window-ms
                v-model="reglages.tens_window_ms" />
            </label>
            <Button data-overlays-change @click="enregistrerReglages">{{ t('change') }}</Button>
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
                v-model="reglages.seek_step_s" />
            </label>
            <Button data-seek-change @click="enregistrerReglages">{{ t('change') }}</Button>
          </CardContent>
        </Card>
      </section>

      <!-- Pochettes. Une seule carte, deux étages qu'il ne faut pas confondre,
           et la mise en page porte cette distinction : le plafond de la source
           vient **en premier** et n'est jamais grisé, parce qu'il s'applique
           quoi que dise l'interrupteur — c'est la seule garde qui subsiste
           quand le réencodage est décoché. L'interrupteur vient ensuite, et
           grise les quatre réglages qui ne décrivent que la vignette.

           Grisés, pas vidés : les valeurs restent lisibles et repartent dans le
           PUT (voir `enregistrerReglages`), donc recocher l'interrupteur
           retrouve ce qu'on avait posé. -->
      <section id="covers" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('cover_card_title') }}</CardTitle></CardHeader>
          <CardContent class="space-y-4">
            <label class="grid gap-1 text-sm">
              {{ t('cover_source_max_label') }}
              <Input type="number" min="1" max="20" class="w-28" data-cover-source-max
                v-model="reglages.cover_source_max_mio" />
              <span class="text-xs text-muted-foreground">{{ t('cover_source_max_help') }}</span>
            </label>

            <div class="border-t border-border pt-4">
              <label class="flex items-start gap-3 text-sm">
                <Switch
                  data-cover-rendition
                  :model-value="reglages.cover_rendition"
                  @update:model-value="(v: boolean) => (reglages.cover_rendition = v)"
                />
                <span class="grid gap-1">
                  {{ t('cover_rendition_label') }}
                  <span class="text-xs text-muted-foreground">{{ t('cover_rendition_help') }}</span>
                </span>
              </label>
            </div>

            <!-- `aria-disabled` en plus du `disabled` de chaque champ : le
                 groupe entier est inactif, et un lecteur d'écran doit pouvoir
                 l'annoncer une fois plutôt que champ par champ. -->
            <div
              data-cover-rendition-group
              :aria-disabled="!reglages.cover_rendition"
              :class="['flex flex-wrap items-start gap-4', reglages.cover_rendition ? '' : 'opacity-50']"
            >
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_edge_label') }}
                <Input type="number" min="64" max="2048" class="w-28" data-cover-max-edge
                  :disabled="!reglages.cover_rendition"
                  v-model="reglages.cover_max_edge_px" />
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_jpeg_quality_label') }}
                <Input type="number" min="40" max="100" class="w-28" data-cover-jpeg-quality
                  :disabled="!reglages.cover_rendition"
                  v-model="reglages.cover_jpeg_quality" />
                <span class="text-xs text-muted-foreground">{{ t('cover_jpeg_quality_help') }}</span>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_bytes_label') }}
                <Input type="number" min="32" max="8192" class="w-28" data-cover-max-bytes
                  :disabled="!reglages.cover_rendition"
                  v-model="reglages.cover_max_bytes_ko" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_bytes_help') }}</span>
              </label>
              <label class="grid gap-1 text-sm">
                {{ t('cover_max_pixels_label') }}
                <Input type="number" min="1" max="64" class="w-28" data-cover-max-pixels
                  :disabled="!reglages.cover_rendition"
                  v-model="reglages.cover_max_pixels_mpx" />
                <span class="text-xs text-muted-foreground">{{ t('cover_max_pixels_help') }}</span>
              </label>
            </div>

            <Button data-cover-change @click="enregistrerReglages">{{ t('change') }}</Button>
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
            @click.prevent="aller(s.id)"
          >
            {{ t(s.key) }}
          </a>
        </li>
      </ul>
    </nav>
  </div>
</template>
