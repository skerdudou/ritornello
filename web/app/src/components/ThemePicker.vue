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

// The four swatches rendered in the displayed mode: a preset is recognised
// far faster by its colours than by its name.
const SWATCHES = ['background', 'primary', 'secondary', 'accent'] as const

function color(id: string, key: string): string {
  const p = presets[id]
  return p ? (resolveVars(p, props.mode)[key] ?? 'transparent') : 'transparent'
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
            v-for="key in SWATCHES"
            :key="key"
            :data-swatch="key"
            class="h-4 w-4 rounded-full border border-border"
            :style="{ background: color(p.id, key) }"
          />
        </span>
      </button>
    </div>
    <p v-if="!list.length" class="text-sm text-muted-foreground">—</p>
  </div>
</template>
