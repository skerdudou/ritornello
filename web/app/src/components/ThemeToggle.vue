<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@ritornello/ui'
import { ColorWheelIcon, MoonIcon, SunIcon } from '@radix-icons/vue'
import { computed, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { useTheme } from '../composables/useTheme'
import ThemePicker from './ThemePicker.vue'

// These labels were the only strings of the shell outside the i18n circuit:
// on a device in French, everything got translated except them. In embedded
// English, the values remain those the e2e journeys target (`getByLabel`).
const { t } = useCatalog()
const { theme, mode, effectiveMode, set, cycleMode } = useTheme()
const open = ref(false)

// Literal keys, not `theme_mode_${mode}`: `i18nKeysUsed.test.ts` only sees a
// key written out in a `t(...)` call.
const modeLabel = computed(() => {
  if (mode.value === 'auto') return t.value('theme_mode_auto')
  return mode.value === 'dark' ? t.value('theme_mode_dark') : t.value('theme_mode_light')
})

async function choose(id: string) {
  await set({ theme: id })
  open.value = false
}
</script>

<template>
  <div class="flex items-center gap-1">
    <!-- Three choices on one key, so the icon says the current state rather
         than the next action (the two-state key showed the sun in dark mode):
         the mode actually painted, plus an "A" when it is the system's doing.
         The state also goes in `title`, for whoever wonders what the A is,
         and after the action in the accessible name, since the A is hidden
         from it (`getByLabel` matches a substring: the journeys still find
         "toggle theme mode"). -->
    <Button
      variant="ghost"
      size="icon"
      :aria-label="`${t('theme_mode_toggle')} (${modeLabel})`"
      :title="modeLabel"
      :data-theme-mode="mode"
      @click="cycleMode()"
    >
      <span class="relative inline-flex">
        <MoonIcon v-if="effectiveMode === 'dark'" class="size-4" />
        <SunIcon v-else class="size-4" />
        <span
          v-if="mode === 'auto'"
          data-theme-mode-auto
          aria-hidden="true"
          class="absolute -right-1.5 -bottom-1.5 rounded-sm bg-background px-px text-[9px] leading-none font-bold"
        >A</span>
      </span>
    </Button>
    <Dialog v-model:open="open">
      <DialogTrigger as-child>
        <Button variant="ghost" size="icon" :aria-label="t('theme_pick')">
          <ColorWheelIcon class="size-4" />
        </Button>
      </DialogTrigger>
      <DialogContent class="sm:max-w-2xl">
        <DialogHeader><DialogTitle>{{ t('theme_title') }}</DialogTitle></DialogHeader>
        <ThemePicker :current="theme" :mode="effectiveMode" @choose="choose" />
      </DialogContent>
    </Dialog>
  </div>
</template>
