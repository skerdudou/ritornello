<script setup lang="ts">
import { Input, presets, resolveVars, type Mode } from '@ritornello/ui'
import { computed, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { filterPresets } from '../composables/useTheme'

const props = defineProps<{ current: string; mode: Mode }>()
defineEmits<{ choose: [id: string] }>()

const { t } = useCatalog()
const query = ref('')
const list = computed(() => filterPresets(query.value))

// Les quatre pastilles rendues dans le mode affiché : un preset se reconnaît
// bien plus vite à ses couleurs qu'à son nom.
const SWATCHES = ['background', 'primary', 'secondary', 'accent'] as const

function color(id: string, cle: string): string {
  const p = presets[id]
  return p ? (resolveVars(p, props.mode)[cle] ?? 'transparent') : 'transparent'
}
</script>

<template>
  <div class="space-y-3">
    <Input v-model="query" :placeholder="t('theme_filter')" />
    <div class="grid max-h-[60vh] grid-cols-2 gap-2 overflow-y-auto sm:grid-cols-3">
      <button
        v-for="p in list"
        :key="p.id"
        :data-preset="p.id"
        :data-active="String(p.id === props.current)"
        class="flex flex-col gap-2 rounded-md border p-2 text-left text-sm"
        :class="p.id === props.current ? 'border-primary ring-1 ring-primary' : 'border-border'"
        @click="$emit('choose', p.id)"
      >
        <span class="truncate">{{ p.label }}</span>
        <span class="flex gap-1">
          <span
            v-for="cle in SWATCHES"
            :key="cle"
            :data-swatch="cle"
            class="h-4 w-4 rounded-full border border-border"
            :style="{ background: color(p.id, cle) }"
          />
        </span>
      </button>
    </div>
    <p v-if="!list.length" class="text-sm text-muted-foreground">—</p>
  </div>
</template>
