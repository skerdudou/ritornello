<script setup lang="ts">
import {
  api, Button, Card, CardAction, CardContent, CardHeader, CardTitle, toast,
} from '@ritornello/ui'
import PlayerCard from '../components/PlayerCard.vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command } from '../types'
import { REMOTE_POWER, REMOTE_ROWS } from './remoteCommands'

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
      <!-- La veille au coin de la carte : c'est la seule commande qui agisse sur
           l'appareil entier plutot que sur la lecture, et la plus consequente —
           la tenir a l'ecart de la grille evite de l'actionner par megarde.
           `CardAction` est ce qui la place a droite **sur la ligne du titre** :
           l'en-tete est une grille qui ne passe en deux colonnes qu'en presence
           de ce slot. Sans lui, le bouton tombait sous le titre. -->
      <CardHeader>
        <CardTitle>{{ t('remote_title') }}</CardTitle>
        <CardAction>
          <Button variant="outline" size="sm" data-remote-power @click="send(REMOTE_POWER.cmd)">
            {{ t(REMOTE_POWER.key) }}
          </Button>
        </CardAction>
      </CardHeader>
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
        <!-- Une rangee par groupe : transport, contenu, son, appareil. Le
             groupement est une donnee (`REMOTE_ROWS`), pas une mise en page
             recopiee ici. -->
        <div v-for="(rangee, i) in REMOTE_ROWS" :key="i" class="flex flex-wrap gap-2" data-remote-row>
          <Button v-for="c in rangee" :key="c.key" variant="outline" @click="send(c.cmd)">
            {{ t(c.key) }}
          </Button>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
