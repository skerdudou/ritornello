<script setup lang="ts">
import { Input, presets, resolveVars, type Mode } from '@ritornello/ui'
import { computed, ref } from 'vue'
import { filterPresets } from '../composables/useTheme'

const props = defineProps<{ current: string; mode: Mode }>()
defineEmits<{ choose: [id: string] }>()

const query = ref('')
const liste = computed(() => filterPresets(query.value))

// Les quatre pastilles rendues dans le mode affiché : un preset se reconnaît
// bien plus vite à ses couleurs qu'à son nom.
const PASTILLES = ['background', 'primary', 'secondary', 'accent'] as const

function couleur(id: string, cle: string): string {
  const p = presets[id]
  return p ? (resolveVars(p, props.mode)[cle] ?? 'transparent') : 'transparent'
}
</script>

<template>
  <div class="space-y-3">
    <Input v-model="query" placeholder="filter" />
    <div class="grid max-h-[60vh] grid-cols-2 gap-2 overflow-y-auto sm:grid-cols-3">
      <button
        v-for="p in liste"
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
            v-for="cle in PASTILLES"
            :key="cle"
            :data-swatch="cle"
            class="h-4 w-4 rounded-full border border-border"
            :style="{ background: couleur(p.id, cle) }"
          />
        </span>
      </button>
    </div>
    <p v-if="!liste.length" class="text-sm text-muted-foreground">—</p>
  </div>
</template>
