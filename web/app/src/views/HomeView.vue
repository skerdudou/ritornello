<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import { LoopIcon } from '@radix-icons/vue'
import { onMounted, ref, watch } from 'vue'
import PresetGrid from '../components/PresetGrid.vue'
import StandbyIcon from '../components/icons/StandbyIcon.vue'
import PlayerCard from '../components/PlayerCard.vue'
import Transport from '../components/Transport.vue'
import Volume from '../components/Volume.vue'
import { useCatalog } from '../composables/useCatalog'
import { usePlayer } from '../composables/usePlayer'
import { usePresets } from '../composables/usePresets'
import type { Command, SettingsPayload } from '../types'
import { unavailable, REMOTE_MUTE, REMOTE_POWER, REMOTE_SOURCE } from './remoteCommands'

const { t } = useCatalog()

// The page's single SSE connection lives here: the Player card, the transport,
// the volume and the grid consume the same state, pushed by `/api/player`.
const { state, ouvre } = usePlayer()
onMounted(ouvre)

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}

// The tile names: loaded on mount, reloaded when the active source changes —
// it is the frame that says so, nothing is probed.
const { reload, nameOf } = usePresets()
onMounted(reload)
watch(() => state.value?.source, (after, before) => {
  if (after !== undefined && after !== before) reload()
})

// The keyboard seek step of the bar: that of the physical keys, served by
// /api/settings. The default covers the duration of the GET and its failure.
const settings = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  startup_power: 'on',
  date_format: 'day_month_year',
  clock_24h: true,
  overlay_ms: 5000,
  tens_window_ms: 5000,
  cover_cache_entries: 20,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_max_bytes_ko: 512,
  cover_max_pixels_mpx: 16,
  seek_step_s: 10,
})
onMounted(async () => {
  settings.value = await api.get<SettingsPayload>('/api/settings').catch(() => settings.value)
})
</script>

<template>
  <!-- One column on a phone; two cards side by side from `md` up. -->
  <div class="grid gap-4 md:grid-cols-2 md:items-start">
    <PlayerCard
      :state="state"
      :seek-step="settings.seek_step_s"
      @seek="(s: number) => send({ cmd: 'SeekTo', arg: s })"
    >
      <!-- The two commands bearing on the whole device, in the corner of the
           card: the source, then standby in the far corner. -->
      <template #actions>
        <div class="flex items-center gap-1">
          <Button
            variant="outline"
            size="sm"
            data-remote-source
            :disabled="unavailable(REMOTE_SOURCE.cmd.cmd, state)"
            @click="send(REMOTE_SOURCE.cmd)"
          >
            <LoopIcon class="size-4" />
            {{ t(REMOTE_SOURCE.key) }}
          </Button>
          <Button variant="outline" size="icon-sm" data-remote-power :aria-label="t(REMOTE_POWER.key)" :title="t(REMOTE_POWER.key)" @click="send(REMOTE_POWER.cmd)">
            <StandbyIcon class="size-4" />
          </Button>
        </div>
      </template>
      <template #commandes>
        <Transport :state="state" @command="send" />
        <Volume
          :volume="state?.volume ?? null"
          :muted="state?.muted ?? false"
          :disabled="unavailable(REMOTE_MUTE.cmd.cmd, state)"
          @set="(v: number) => send({ cmd: 'SetVolume', arg: v })"
          @mute="send(REMOTE_MUTE.cmd)"
        />
      </template>
    </PlayerCard>
    <Card>
      <CardHeader><CardTitle>{{ t('presets_label') }}</CardTitle></CardHeader>
      <CardContent>
        <PresetGrid :state="state" :name-of="(n: number) => (state ? nameOf(state.source, n) : null)" @choose="(n: number) => send({ cmd: 'Select', arg: n })" />
      </CardContent>
    </Card>
  </div>
</template>
