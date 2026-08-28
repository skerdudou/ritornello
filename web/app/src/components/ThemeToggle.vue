<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@ritornello/ui'
import { ColorWheelIcon, MoonIcon, SunIcon } from '@radix-icons/vue'
import { ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { useTheme } from '../composables/useTheme'
import ThemePicker from './ThemePicker.vue'

// Ces libellés étaient les seules chaînes du shell hors du circuit i18n : sur
// un appareil en français, tout se traduisait sauf eux. En anglais embarqué,
// les valeurs restent celles que les journey e2e ciblent (`getByLabel`).
const { t } = useCatalog()
const { theme, mode, set, toggleMode } = useTheme()
const open = ref(false)

async function choose(id: string) {
  await set({ theme: id })
  open.value = false
}
</script>

<template>
  <div class="flex items-center gap-1">
    <Button variant="ghost" size="icon" :aria-label="t('theme_mode_toggle')" @click="toggleMode()">
      <SunIcon v-if="mode === 'dark'" class="size-4" />
      <MoonIcon v-else class="size-4" />
    </Button>
    <Dialog v-model:open="open">
      <DialogTrigger as-child>
        <Button variant="ghost" size="icon" :aria-label="t('theme_pick')">
          <ColorWheelIcon class="size-4" />
        </Button>
      </DialogTrigger>
      <DialogContent class="sm:max-w-2xl">
        <DialogHeader><DialogTitle>{{ t('theme_title') }}</DialogTitle></DialogHeader>
        <ThemePicker :current="theme" :mode="mode" @choose="choose" />
      </DialogContent>
    </Dialog>
  </div>
</template>
