<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import PlayerCard from '../components/PlayerCard.vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command } from '../types'
import { REMOTE_COMMANDS } from './remoteCommands'

const { t } = useCatalog()

const PRESETS = [1, 2, 3, 4, 5, 6, 7, 8, 9]

// La source active vient desormais du flux pousse `/api/player`, avec le reste
// de l'etat volatil, et non d'une lecture unique de `/api/status` au montage :
// elle changeait sans que la page le sache (telecommande infrarouge, autre
// onglet, bouton de l'appareil).

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}
</script>

<template>
  <div class="space-y-4">
    <PlayerCard />
    <Card>
      <CardHeader><CardTitle>{{ t('remote_title') }}</CardTitle></CardHeader>
      <CardContent class="space-y-3">
        <div class="grid grid-cols-3 gap-2 sm:grid-cols-9">
          <Button
            v-for="n in PRESETS"
            :key="n"
            :data-preset-button="n"
            variant="secondary"
            @click="send({ cmd: 'Select', arg: n })"
          >
            {{ n }}
          </Button>
        </div>
        <div class="flex flex-wrap gap-2">
          <Button v-for="c in REMOTE_COMMANDS" :key="c.key" variant="outline" @click="send(c.cmd)">
            {{ t(c.key) }}
          </Button>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
