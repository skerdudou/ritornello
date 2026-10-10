<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import { LoopIcon } from '@radix-icons/vue'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import PresetGrid from '../components/PresetGrid.vue'
import SourceKeys from '../components/SourceKeys.vue'
import StandbyIcon from '../components/icons/StandbyIcon.vue'
import PlayerCard from '../components/PlayerCard.vue'
import Transport from '../components/Transport.vue'
import Volume from '../components/Volume.vue'
import { useCatalog } from '../composables/useCatalog'
import { usePlayer } from '../composables/usePlayer'
import { usePresets } from '../composables/usePresets'
import type { Command, SettingsPayload } from '../types'
import { headerMode } from './headerMode'
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
const { reload, nameOf, listedCount, sources, icons, listRead } = usePresets()
onMounted(reload)
watch(() => state.value?.source, (after, before) => {
  if (after !== undefined && after !== before) reload()
})

// What the Player card's header offers to change source: one icon key per
// source when they fit, the cycle key when they do not or when the page does
// not know yet, nothing for a single source (see `headerMode`). The width is
// the header's own, measured, not a breakpoint: what fits depends on how many
// sources there are, which no breakpoint knows.
const playerCard = ref<{ headerElement: () => HTMLElement | null } | null>(null)
const available = ref<number | null>(null)
let observer: ResizeObserver | null = null
onMounted(() => {
  const header = playerCard.value?.headerElement()
  if (!header) return
  // The content box: the header's padding is not room for the keys.
  observer = new ResizeObserver((entries) => {
    const entry = entries[entries.length - 1]
    if (entry) available.value = entry.contentRect.width
  })
  observer.observe(header)
})
onUnmounted(() => observer?.disconnect())
const mode = computed(() =>
  headerMode({
    sources: sources.value.length,
    listRead: listRead.value,
    available: available.value,
    standby: state.value?.standby ?? false,
    activeListed: !state.value?.source || sources.value.includes(state.value.source),
  }),
)

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
  cover_cache_budget_mio: 50,
  cover_download_max_mio: 2,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_passthrough_max_ko: 150,
  cover_max_pixels_mpx: 16,
  seek_step_s: 10,
  update_policy: 'off',
  update_hour: 3,
  update_cadence: { kind: 'daily' },
  update_prereleases: false,
})
onMounted(async () => {
  settings.value = await api.get<SettingsPayload>('/api/settings').catch(() => settings.value)
})
</script>

<template>
  <!-- One column on a phone; two cards side by side from `md` up. -->
  <div class="grid gap-4 md:grid-cols-2 md:items-start">
    <PlayerCard
      ref="playerCard"
      :state="state"
      :show-source="mode !== 'icons'"
      :seek-step="settings.seek_step_s"
      @seek="(s: number) => send({ cmd: 'SeekTo', arg: s })"
    >
      <!-- The two commands bearing on the whole device, in the corner of the
           card: the source, then standby in the far corner. The source is
           one key per source when they fit, the cycle key otherwise, nothing
           when there is only one. -->
      <template #actions>
        <div class="flex items-center gap-1">
          <SourceKeys
            v-if="mode === 'icons'"
            :sources="sources"
            :icons="icons"
            :active="state?.source ?? null"
            :playing="state?.playback === 'playing'"
            :disabled="unavailable('SelectSource', state)"
            @choose="(name: string) => send({ cmd: 'SelectSource', arg: name })"
          />
          <Button
            v-else-if="mode === 'cycle'"
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
        <PresetGrid :state="state" :listed-count="state ? listedCount(state.source) : 0" :name-of="(n: number) => (state ? nameOf(state.source, n) : null)" @choose="(n: number) => send({ cmd: 'Select', arg: n })" />
      </CardContent>
    </Card>
  </div>
</template>
