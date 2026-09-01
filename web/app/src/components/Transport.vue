<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { PauseIcon, PlayIcon, StopIcon, TrackNextIcon, TrackPreviousIcon } from '@radix-icons/vue'
import type { Component } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command, PlayerPayload } from '../types'
import {
  unavailable, hidden, REMOTE_TRANSPORT, REMOTE_TRANSPORT_SECONDARY,
} from '../views/remoteCommands'
import type { RemoteCommand } from '../views/remoteCommands'
import EjectIcon from './icons/EjectIcon.vue'

// The transport as icons, play alone in full: it is the frequent gesture, and
// the eye finds it without reading. The order is that of `REMOTE_TRANSPORT`.
const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null }>()
const emit = defineEmits<{ command: [cmd: Command] }>()

const ICONS: Record<string, Component> = {
  Prev: TrackPreviousIcon,
  Next: TrackNextIcon,
  Stop: StopIcon,
  Eject: EjectIcon,
}

function icon(c: RemoteCommand): Component {
  if (c.cmd.cmd === 'PlayPause') return props.state?.playback === 'playing' ? PauseIcon : PlayIcon
  return ICONS[c.cmd.cmd] ?? PlayIcon
}

const visible = (list: RemoteCommand[]) => list.filter((c) => !hidden(c.cmd.cmd, props.state))
</script>

<template>
  <!-- **The main group is centred, not the whole row — and at every width.**
       The five buttons were all direct children of a `justify-center`: the
       secondary group (stop, eject) therefore counted in the centring, and
       previous/play/next ended up shifted to the left by half its width.

       An equally flexible void on the left (`flex-1` on both sides) gives the
       trio the middle of the card back. It has **no** `md:hidden`: on PC the
       row aligned left, which left the trio glued to the edge and Stop lost
       at the other end — the owner reported it on a screenshot. The trio is
       now in the middle everywhere, and the secondary group stays set back on
       the right, where its flexible column pushes it. -->
  <div class="flex items-center" data-transport>
    <span class="flex-1" aria-hidden="true" />
    <div class="flex items-center gap-3 md:gap-2">
      <Button
        v-for="c in visible(REMOTE_TRANSPORT)"
        :key="c.key"
        :data-remote-command="c.cmd.cmd"
        :data-playback="c.cmd.cmd === 'PlayPause' ? (state?.playback ?? 'stopped') : undefined"
        :variant="c.cmd.cmd === 'PlayPause' ? 'default' : 'ghost'"
        :class="c.cmd.cmd === 'PlayPause' ? 'size-16 rounded-full md:size-12' : 'size-12 rounded-full md:size-10'"
        :aria-label="t(c.key)"
        :title="t(c.key)"
        :disabled="unavailable(c.cmd.cmd, state)"
        @click="emit('command', c.cmd)"
      >
        <component :is="icon(c)" :class="c.cmd.cmd === 'PlayPause' ? 'size-7 md:size-6' : 'size-6 md:size-5'" />
      </Button>
    </div>
    <!-- Set back, on the right: its column has the same flexibility as the
         void on the left, which is what keeps the trio in the middle. -->
    <div class="flex flex-1 items-center justify-end gap-1">
      <Button
        v-for="c in visible(REMOTE_TRANSPORT_SECONDARY)"
        :key="c.key"
        :data-remote-command="c.cmd.cmd"
        variant="ghost"
        class="size-12 rounded-full text-muted-foreground md:size-10"
        :aria-label="t(c.key)"
        :title="t(c.key)"
        :disabled="unavailable(c.cmd.cmd, state)"
        @click="emit('command', c.cmd)"
      >
        <component :is="icon(c)" class="size-5" />
      </Button>
    </div>
  </div>
</template>
