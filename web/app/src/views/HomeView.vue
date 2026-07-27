<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import { onMounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command, StatusPayload } from '../types'
import { REMOTE_COMMANDS } from './remoteCommands'

const { t } = useCatalog()
const active = ref('')

const PRESETS = [1, 2, 3, 4, 5, 6, 7, 8, 9]

onMounted(async () => {
  // Le catalogue est déjà chargé par `App.vue` (état de module partagé) :
  // pas besoin de le recharger ici.
  const s = await api.get<StatusPayload>('/api/status').catch(() => null)
  if (s) active.value = s.active_source
})

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}
</script>

<template>
  <div class="space-y-4">
    <p class="text-sm text-muted-foreground">
      {{ t('active_source_label') }} : <span class="text-foreground">{{ active }}</span>
    </p>
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
