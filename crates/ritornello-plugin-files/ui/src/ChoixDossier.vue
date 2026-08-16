<script setup lang="ts">
import { Button } from '@ritornello/ui'
import type { Exploration, T } from './donnees'

/**
 * L'arbre de choix des deux assistants.
 *
 * Partagé, parce que descendre dans des dossiers est le même geste des deux
 * côtés : seule la machine qui répond change. Il n'émet que des **noms** de
 * dossier, jamais des chemins — la composition du chemin appartient à
 * l'appelant, un chemin local et un chemin SMB ne se composant pas pareil.
 */
defineProps<{ exploration: Exploration; t: T; fige: boolean }>()
defineEmits<{ descendre: [nom: string]; remonter: [] }>()
</script>

<template>
  <div class="space-y-2" data-choix>
    <div class="flex items-center gap-2 text-sm">
      <Button
        variant="ghost"
        size="sm"
        data-choix-remonter
        :disabled="fige || exploration.busy"
        @click="$emit('remonter')"
      >
        ↑ {{ t('btn_up') }}
      </Button>
      <span class="truncate text-muted-foreground" data-choix-chemin>
        {{ exploration.path || '/' }}
      </span>
    </div>

    <!-- Le refus remplace l'arbre : l'afficher vide en dessous laisserait
         croire que le dossier existe et qu'il est vide. -->
    <p v-if="exploration.error" class="text-sm text-destructive" data-choix-erreur>
      {{ exploration.error }}
    </p>

    <template v-else>
      <p v-if="exploration.busy" class="text-sm text-muted-foreground" data-choix-busy>
        {{ t('connecting') }}
      </p>

      <ul class="max-h-64 space-y-1 overflow-y-auto text-sm">
        <li v-for="d in exploration.dirs" :key="d">
          <button
            type="button"
            data-choix-dossier
            class="w-full truncate rounded px-2 py-1 text-left hover:bg-accent"
            :disabled="fige || exploration.busy"
            @click="$emit('descendre', d)"
          >
            📁 {{ d }}
          </button>
        </li>
        <li
          v-if="!exploration.dirs.length && !exploration.busy"
          class="px-2 text-muted-foreground"
          data-choix-vide
        >
          {{ t('empty_folder') }}
        </li>
      </ul>

      <!-- Le compte de fichiers audio du niveau ouvert : c'est lui qui dit
           qu'on est au bon endroit. Sans lui, on choisit un dossier en
           espérant. -->
      <p class="text-sm text-muted-foreground" data-audio-count>
        {{ t('audio_here', { count: exploration.audioCount }) }}
      </p>
    </template>
  </div>
</template>
