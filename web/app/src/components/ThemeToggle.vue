<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@ritornello/ui'
import { ColorWheelIcon, MoonIcon, SunIcon } from '@radix-icons/vue'
import { ref } from 'vue'
import { useTheme } from '../composables/useTheme'
import ThemePicker from './ThemePicker.vue'

const { theme, mode, set, toggleMode } = useTheme()
const ouvert = ref(false)

async function choisir(id: string) {
  await set({ theme: id })
  ouvert.value = false
}
</script>

<template>
  <div class="flex items-center gap-1">
    <Button variant="ghost" size="icon" aria-label="toggle theme mode" @click="toggleMode()">
      <SunIcon v-if="mode === 'dark'" class="size-4" />
      <MoonIcon v-else class="size-4" />
    </Button>
    <Dialog v-model:open="ouvert">
      <DialogTrigger as-child>
        <Button variant="ghost" size="icon" aria-label="pick theme">
          <ColorWheelIcon class="size-4" />
        </Button>
      </DialogTrigger>
      <DialogContent class="sm:max-w-2xl">
        <DialogHeader><DialogTitle>Theme</DialogTitle></DialogHeader>
        <ThemePicker :current="theme" :mode="mode" @choose="choisir" />
      </DialogContent>
    </Dialog>
  </div>
</template>
