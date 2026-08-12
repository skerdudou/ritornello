<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle, Input,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'
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
  // Repli sur le premier peripherique disponible, jamais la chaine vide.
  // L'ancienne page etait rendue cote serveur : faute de sortie choisie, aucun
  // `<option>` ne portait `selected`, donc le navigateur selectionnait le
  // premier peripherique et « Changer » envoyait toujours un nom reel. Sur une
  // installation neuve (`current: null`), `?? ''` laissait au contraire le
  // declencheur vide et « Changer » envoyait `device: ""`. Le coeur le refuse
  // desormais (422, voir `status::validate_audio_device`) ; ce repli evite de
  // proposer a l'utilisateur un bouton qui ne peut que rater.
  device.value = audio.value.current ?? audio.value.devices[0] ?? ''
  lang.value = locale.value.current ?? 'en'
}

onMounted(chargerTout)

async function changerSortie() {
  const err = await api.put('/api/audio-output', { device: device.value })
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
</script>

<template>
  <div class="space-y-4">
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

    <Card>
      <CardHeader><CardTitle>{{ t('audio_output') }}</CardTitle></CardHeader>
      <CardContent class="flex flex-wrap items-center gap-2">
        <!-- Le titre de la carte n'est pas associé au déclencheur : sans
             aria-label, le sélecteur n'a aucun nom accessible. -->
        <Select v-model="device">
          <SelectTrigger class="min-w-64" :aria-label="t('audio_output')"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem v-for="d in audio.devices" :key="d" :value="d">{{ d }}</SelectItem>
          </SelectContent>
        </Select>
        <Button data-audio-change @click="changerSortie">{{ t('change') }}</Button>
      </CardContent>
    </Card>

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

    <Card>
      <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
      <CardContent>
        <ul class="space-y-1 font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in logs" :key="i" data-log-line>{{ l }}</li>
        </ul>
      </CardContent>
    </Card>
  </div>
</template>
