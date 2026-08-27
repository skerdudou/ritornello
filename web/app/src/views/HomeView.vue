<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import { LoopIcon } from '@radix-icons/vue'
import { onMounted, ref, watch } from 'vue'
import GrillePresets from '../components/GrillePresets.vue'
import IconeVeille from '../components/icones/IconeVeille.vue'
import PlayerCard from '../components/PlayerCard.vue'
import Transport from '../components/Transport.vue'
import Volume from '../components/Volume.vue'
import { useCatalog } from '../composables/useCatalog'
import { usePlayer } from '../composables/usePlayer'
import { usePresets } from '../composables/usePresets'
import type { Command, SettingsPayload } from '../types'
import { indisponible, REMOTE_MUTE, REMOTE_POWER, REMOTE_SOURCE } from './remoteCommands'

const { t } = useCatalog()

// L'unique connexion SSE de la page vit ici : la carte Lecteur, le transport,
// le volume et la grille consomment le meme etat, pousse par `/api/player`.
const { etat, ouvre } = usePlayer()
onMounted(ouvre)

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}

// Les noms des tuiles : charges au montage, recharges quand la source active
// change — c'est la trame qui le dit, rien n'est sonde.
const { recharger, nomDe } = usePresets()
onMounted(recharger)
watch(() => etat.value?.source, (apres, avant) => {
  if (apres !== undefined && apres !== avant) recharger()
})

// Pas de deplacement au clavier de la barre : celui des touches physiques,
// servi par /api/settings. Le defaut couvre le temps du GET et son echec.
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  startup_power: 'on',
  overlay_ms: 5000,
  tens_window_ms: 5000,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_max_bytes_ko: 512,
  cover_max_pixels_mpx: 16,
  seek_step_s: 10,
})
onMounted(async () => {
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
})
</script>

<template>
  <!-- Une colonne sur telephone ; deux cartes cote a cote a partir de `md`. -->
  <div class="grid gap-4 md:grid-cols-2 md:items-start">
    <PlayerCard
      :etat="etat"
      :pas-deplacement="reglages.seek_step_s"
      @deplacer="(s: number) => send({ cmd: 'SeekTo', arg: s })"
    >
      <!-- Les deux commandes qui portent sur l'appareil entier, au coin de la
           carte : la source, puis la veille au coin extreme. -->
      <template #actions>
        <div class="flex items-center gap-1">
          <Button
            variant="outline"
            size="sm"
            data-remote-source
            :disabled="indisponible(REMOTE_SOURCE.cmd.cmd, etat)"
            @click="send(REMOTE_SOURCE.cmd)"
          >
            <LoopIcon class="size-4" />
            {{ t(REMOTE_SOURCE.key) }}
          </Button>
          <Button variant="outline" size="icon-sm" data-remote-power :aria-label="t(REMOTE_POWER.key)" :title="t(REMOTE_POWER.key)" @click="send(REMOTE_POWER.cmd)">
            <IconeVeille class="size-4" />
          </Button>
        </div>
      </template>
      <template #commandes>
        <Transport :etat="etat" @commande="send" />
        <Volume
          :volume="etat?.volume ?? null"
          :muted="etat?.muted ?? false"
          :desactive="indisponible(REMOTE_MUTE.cmd.cmd, etat)"
          @regler="(v: number) => send({ cmd: 'SetVolume', arg: v })"
          @muet="send(REMOTE_MUTE.cmd)"
        />
      </template>
    </PlayerCard>
    <Card>
      <CardHeader><CardTitle>{{ t('presets_label') }}</CardTitle></CardHeader>
      <CardContent>
        <GrillePresets :etat="etat" :nom-de="(n: number) => (etat ? nomDe(etat.source, n) : null)" @choisir="(n: number) => send({ cmd: 'Select', arg: n })" />
      </CardContent>
    </Card>
  </div>
</template>
