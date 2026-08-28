<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { PauseIcon, PlayIcon, StopIcon, TrackNextIcon, TrackPreviousIcon } from '@radix-icons/vue'
import type { Component } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command, PlayerPayload } from '../types'
import {
  indisponible, masquee, REMOTE_TRANSPORT, REMOTE_TRANSPORT_SECONDAIRE,
} from '../views/remoteCommands'
import type { RemoteCommand } from '../views/remoteCommands'
import IconeEjecter from './icones/IconeEjecter.vue'

// Le transport en icones, la lecture seule en plein : c'est le geste frequent,
// et l'oeil la trouve sans lire. L'ordre est celui de `REMOTE_TRANSPORT`.
const { t } = useCatalog()
const props = defineProps<{ etat: PlayerPayload | null }>()
const emit = defineEmits<{ commande: [cmd: Command] }>()

const ICONES: Record<string, Component> = {
  Prev: TrackPreviousIcon,
  Next: TrackNextIcon,
  Stop: StopIcon,
  Eject: IconeEjecter,
}

function icone(c: RemoteCommand): Component {
  if (c.cmd.cmd === 'PlayPause') return props.etat?.playback === 'playing' ? PauseIcon : PlayIcon
  return ICONES[c.cmd.cmd] ?? PlayIcon
}

const visibles = (liste: RemoteCommand[]) => liste.filter((c) => !masquee(c.cmd.cmd, props.etat))
</script>

<template>
  <!-- **Le groupe principal est centre, pas la rangee entiere — et a toutes
       les largeurs.** Les cinq boutons etaient tous enfants directs d'un
       `justify-center` : le groupe secondaire (arret, ejection) comptait donc
       dans le centrage, et precedent/lecture/suivant se retrouvaient decales
       vers la gauche de la moitie de sa largeur.

       Un vide de meme souplesse a gauche (`flex-1` des deux cotes) rend au
       trio le milieu de la carte. Il n'a **pas** de `md:hidden` : sur PC la
       rangee s'alignait a gauche, ce qui laissait le trio colle au bord et
       Arret perdu a l'autre bout — le proprietaire l'a signale sur capture. Le
       trio est desormais au milieu partout, et le groupe secondaire reste en
       retrait a droite, la ou sa colonne souple le pousse. -->
  <div class="flex items-center" data-transport>
    <span class="flex-1" aria-hidden="true" />
    <div class="flex items-center gap-3 md:gap-2">
      <Button
        v-for="c in visibles(REMOTE_TRANSPORT)"
        :key="c.key"
        :data-remote-command="c.cmd.cmd"
        :data-playback="c.cmd.cmd === 'PlayPause' ? (etat?.playback ?? 'stopped') : undefined"
        :variant="c.cmd.cmd === 'PlayPause' ? 'default' : 'ghost'"
        :class="c.cmd.cmd === 'PlayPause' ? 'size-16 rounded-full md:size-12' : 'size-12 rounded-full md:size-10'"
        :aria-label="t(c.key)"
        :title="t(c.key)"
        :disabled="indisponible(c.cmd.cmd, etat)"
        @click="emit('commande', c.cmd)"
      >
        <component :is="icone(c)" :class="c.cmd.cmd === 'PlayPause' ? 'size-7 md:size-6' : 'size-6 md:size-5'" />
      </Button>
    </div>
    <!-- En retrait, a droite : sa colonne a la meme souplesse que le vide de
         gauche, c'est ce qui garde le trio au milieu. -->
    <div class="flex flex-1 items-center justify-end gap-1">
      <Button
        v-for="c in visibles(REMOTE_TRANSPORT_SECONDAIRE)"
        :key="c.key"
        :data-remote-command="c.cmd.cmd"
        variant="ghost"
        class="size-12 rounded-full text-muted-foreground md:size-10"
        :aria-label="t(c.key)"
        :title="t(c.key)"
        :disabled="indisponible(c.cmd.cmd, etat)"
        @click="emit('commande', c.cmd)"
      >
        <component :is="icone(c)" class="size-5" />
      </Button>
    </div>
  </div>
</template>
