<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { computed } from 'vue'
import { truncateStart, type Exploration, type T } from './data'

/**
 * L'arbre de choix des deux assistants.
 *
 * Partagé, parce que descend dans des dossiers est le même geste des deux
 * côtés : seule la machine qui répond change. Il n'émet que des **noms** de
 * dossier, jamais des chemins — la composition du path appartient à
 * l'appelant, un path local et un path SMB ne se composant pas pareil.
 */
const props = defineProps<{
  exploration: Exploration
  t: T
  fige: boolean
  /**
   * Chemin **à afficher**, fourni par l'appelant.
   *
   * Il n'est plus déduit de `exploration.path`, et c'est le correctif d'un
   * défaut signalé : côté partage, ce champ est relatif au partage, si bien que
   * le partage choisi n'apparaissait nulle part — on le voyait « disparaître »
   * en y entrant. Seul l'appelant sait composer l'adresse complète :
   * `//hôte/partage/...` d'un côté, path absolu de l'autre.
   */
  path: string
}>()
defineEmits<{ descend: [nom: string]; goUp: [] }>()

/**
 * Le path, tronqué **par le début** pour tenir dans la ligne.
 *
 * L'infobulle porte la valeur entière : tronquer sert à faire tenir, pas à
 * cacher.
 */
const shortPath = computed(() => truncateStart(props.path))
</script>

<template>
  <!-- `min-w-0` partout où du texte long descend, et ce n'est pas décoratif :
       la largeur minimale d'un enfant de grille ou de flex vaut par défaut celle
       de son contenu. Un path ou un nom de dossier long poussait donc la boîte
       de dialogue au-delà de son propre fond, et la barre de défilement comme
       les boutons se retrouvaient peints hors du cadre blanc. C'est aussi ce qui
       rend `truncate` opérant : sans autorisation de rétrécir, il n'a jamais
       l'occasion de couper. -->
  <div class="min-w-0 space-y-2" data-choix>
    <div class="flex min-w-0 items-center gap-2 text-sm">
      <Button
        variant="ghost"
        size="sm"
        class="shrink-0"
        data-choix-goUp
        :disabled="fige || exploration.busy"
        @click="$emit('goUp')"
      >
        ↑ {{ t('btn_up') }}
      </Button>
      <!-- Tronqué **par le début**, par `truncateStart` : sur un path,
           l'information utile est la fin, et aucune propriété CSS ne sait couper
           de ce côté-là. Le titre porte le path entier. -->
      <span class="min-w-0 flex-1 truncate text-muted-foreground" data-choix-path :title="path">
        {{ shortPath }}
      </span>
    </div>

    <!-- Le refus remplace l'arbre : l'afficher vide en dessous laisserait
         croire que le dossier existe et qu'il est vide. -->
    <p v-if="exploration.error" class="min-w-0 break-words text-sm text-destructive" data-choix-erreur>
      {{ exploration.error }}
    </p>

    <template v-else>
      <p v-if="exploration.busy" class="text-sm text-muted-foreground" data-choix-busy>
        {{ t('connecting') }}
      </p>

      <ul class="max-h-64 min-w-0 space-y-1 overflow-y-auto text-sm">
        <li v-for="d in exploration.dirs" :key="d" class="min-w-0">
          <button
            type="button"
            data-choix-dossier
            class="block w-full truncate rounded px-2 py-1 text-left hover:bg-accent"
            :disabled="fige || exploration.busy"
            :title="d"
            @click="$emit('descend', d)"
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
