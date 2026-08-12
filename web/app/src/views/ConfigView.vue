<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle, Input,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { nomLangue } from '../composables/langues'
import { useCatalog } from '../composables/useCatalog'
import type { AudioPayload, LocalePayload, LogsPayload, SettingsPayload, StatusPayload } from '../types'

const { t, reload } = useCatalog()
const status = ref<StatusPayload>({ plugins: [], active_source: '' })
const audio = ref<AudioPayload>({ devices: [], current: null })
const locale = ref<LocalePayload>({ locales: [], current: null })
const logs = ref<string[]>([])
const device = ref('')
const lang = ref('')
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 1000,
  volume_repeat_interval_ms: 500,
  start_in_standby: false,
})
// Le Select ne porte que des chaînes : « on »/« standby » traduits à
// l'affichage, le booléen reste la valeur envoyée au cœur.
const demarrage = computed({
  get: () => (reglages.value.start_in_standby ? 'standby' : 'on'),
  set: (v: string) => { reglages.value.start_in_standby = v === 'standby' },
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
  status.value = await api.get<StatusPayload>('/api/status').catch(() => status.value)
  audio.value = await api.get<AudioPayload>('/api/audio-output').catch(() => audio.value)
  locale.value = await api.get<LocalePayload>('/api/locale').catch(() => locale.value)
  logs.value = (await api.get<LogsPayload>('/api/logs').catch(() => ({ lines: [] }))).lines
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
  // `current: null` = aucun choix enregistré : c'est l'entrée « Par défaut
  // (système) » qui le porte — plus de repli sur le premier périphérique
  // (c'était `null`, le PCM qui jette le son, en tête de `aplay -L`).
  device.value = audio.value.current ?? DEFAUT_SYSTEME
  lang.value = locale.value.current ?? 'en'
}

onMounted(chargerTout)

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
  { id: 'logs', key: 'recent_errors' },
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
                </tr>
              </thead>
              <tbody>
                <tr v-for="p in status.plugins" :key="p.name" data-plugin-row class="border-t border-border">
                  <td class="py-1" data-plugin-name>{{ p.name }}</td>
                  <td data-plugin-kind>{{ p.kind }}</td>
                  <td data-plugin-state>
                    <Badge :variant="p.connected ? 'secondary' : 'destructive'">
                      {{ p.connected ? t('connected') : t('unavailable') }}
                    </Badge>
                  </td>
                  <td>
                    <RouterLink v-if="p.admin" :to="`/plugins/${p.name}/`" data-admin-link class="underline">
                      {{ t('admin_link') }}
                    </RouterLink>
                    <span v-else>-</span>
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
            <Button data-audio-change @click="changerSortie">{{ t('change') }}</Button>
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
            <Select v-model="demarrage">
              <SelectTrigger class="min-w-32" data-startup-select :aria-label="t('startup_title')"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="on">{{ t('startup_on') }}</SelectItem>
                <SelectItem value="standby">{{ t('startup_standby') }}</SelectItem>
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

      <section id="logs" class="scroll-mt-6">
        <Card>
          <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
          <CardContent>
            <ul class="space-y-1 font-mono text-xs text-muted-foreground">
              <li v-for="(l, i) in logs" :key="i" data-log-line>{{ l }}</li>
            </ul>
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
