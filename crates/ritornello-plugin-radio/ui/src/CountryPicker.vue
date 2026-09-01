<script setup lang="ts">
import { Input } from '@ritornello/ui'
import { computed, ref } from 'vue'
import { countryName, displayableCountries, ALL_COUNTRIES, type Country } from './country'

const props = defineProps<{
  /** List returned by the plugin. Empty = not yet fetched, or unreachable directory. */
  list: Country[]
  /** Selected code, `''` for "all countries". */
  current: string
  /** Labels provided by the caller, which holds the catalog. */
  allLabel: string
  placeholder: string
  emptyLabel: string
}>()
defineEmits<{ choose: [code: string] }>()

const filter = ref('')
const displayable = computed(() => displayableCountries(props.list, filter.value))
</script>

<template>
  <div class="space-y-3">
    <Input v-model="filter" data-country-filter :placeholder="placeholder" />
    <div class="max-h-[60vh] space-y-1 overflow-y-auto">
      <!-- "All countries" is never filtered out: it is the way back, it must
           stay reachable regardless of the text typed. -->
      <button
        :data-country="ALL_COUNTRIES || 'ALL'"
        :data-active="String(props.current === ALL_COUNTRIES)"
        class="flex w-full items-center justify-between rounded-md border px-2 py-1.5 text-left text-sm"
        :class="props.current === ALL_COUNTRIES ? 'border-primary ring-1 ring-primary' : 'border-border'"
        @click="$emit('choose', ALL_COUNTRIES)"
      >
        <span>{{ allLabel }}</span>
      </button>
      <button
        v-for="p in displayable"
        :key="p.code"
        :data-country="p.code"
        :data-active="String(p.code === props.current)"
        class="flex w-full items-center justify-between gap-2 rounded-md border px-2 py-1.5 text-left text-sm"
        :class="p.code === props.current ? 'border-primary ring-1 ring-primary' : 'border-border'"
        @click="$emit('choose', p.code)"
      >
        <span class="truncate">{{ p.name }}</span>
        <!-- The station count helps in choosing: a country with eight
             stations won't give much. -->
        <span class="shrink-0 text-xs tabular-nums text-muted-foreground">{{ p.stations }}</span>
      </button>
      <p v-if="!displayable.length" class="text-sm text-muted-foreground" data-country-empty>
        {{ emptyLabel }}
      </p>
    </div>
  </div>
</template>
