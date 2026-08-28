<script setup lang="ts">
import { Input } from '@ritornello/ui'
import { computed, ref } from 'vue'
import { countryName, displayableCountries, ALL_COUNTRIES, type Country } from './country'

const props = defineProps<{
  /** Liste renvoyee par le plugin. Vide = pas encore recuperee, ou annuaire injoignable. */
  liste: Country[]
  /** Code selectionne, `''` pour « tous les country ». */
  current: string
  /** Libelles fournis par l'appelant, qui a le catalogue. */
  labelTous: string
  placeholder: string
  vide: string
}>()
defineEmits<{ choose: [code: string] }>()

const filter = ref('')
const displayable = computed(() => displayableCountries(props.liste, filter.value))
</script>

<template>
  <div class="space-y-3">
    <Input v-model="filter" data-country-filter :placeholder="placeholder" />
    <div class="max-h-[60vh] space-y-1 overflow-y-auto">
      <!-- « Tous les country » n'est jamais filter : c'est le moyen de revenir en
           arriere, il doit rester atteignable quel que soit le texte saisi. -->
      <button
        :data-country="ALL_COUNTRIES || 'ALL'"
        :data-active="String(props.current === ALL_COUNTRIES)"
        class="flex w-full items-center justify-between rounded-md border px-2 py-1.5 text-left text-sm"
        :class="props.current === ALL_COUNTRIES ? 'border-primary ring-1 ring-primary' : 'border-border'"
        @click="$emit('choose', ALL_COUNTRIES)"
      >
        <span>{{ labelTous }}</span>
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
        <span class="truncate">{{ p.nom }}</span>
        <!-- Le nombre de stations aide a choisir : un country a huit stations ne
             donnera pas grand-chose. -->
        <span class="shrink-0 text-xs tabular-nums text-muted-foreground">{{ p.stations }}</span>
      </button>
      <p v-if="!displayable.length" class="text-sm text-muted-foreground" data-country-empty>
        {{ vide }}
      </p>
    </div>
  </div>
</template>
