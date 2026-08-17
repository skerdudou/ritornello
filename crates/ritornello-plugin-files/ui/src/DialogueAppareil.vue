<script setup lang="ts">
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@ritornello/ui'
import { computed } from 'vue'
import ChoixDossier from './ChoixDossier.vue'
import type { Donnees, Envoyer, T } from './donnees'

/**
 * Assistant « dossier de l'appareil ».
 *
 * Il ouvre sur la liste des volumes, jamais sur `/` : le chemin absolu d'une
 * clé USB n'est connu de personne, et c'est précisément ce que l'ancien
 * formulaire demandait de taper.
 */
const props = defineProps<{
  donnees: Donnees
  t: T
  envoyer: Envoyer
  fige: boolean
  ouvert: boolean
  /**
   * Dernier refus du plugin, tel que la page l'a reçu.
   *
   * Il arrive ici parce qu'un refus né dans la popin doit s'y afficher : le
   * bandeau de la page principale est derrière le voile gris de la boîte de
   * dialogue, donc à peu près invisible au moment où il compte le plus.
   */
  message: string
}>()
const emit = defineEmits<{ fermer: [] }>()

const ex = computed(() => props.donnees.explore)
/** Un volume a été choisi : on est dans l'arbre plutôt que dans la liste. */
const dansLArbre = computed(() => ex.value.kind === 'local' && ex.value.path !== '')

/**
 * Volume contenant le chemin courant : le plus long préfixe déclaré.
 *
 * Sert à borner la remontée. Sans lui, « remonter » depuis le sommet d'une clé
 * USB emmenait dans `/media`, puis `/` — on sortait du volume qu'on venait de
 * choisir, sans jamais retrouver la liste.
 */
const volumeCourant = computed(() => {
  const sous = (base: string) =>
    ex.value.path === base || ex.value.path.startsWith(`${base.replace(/\/$/, '')}/`)
  return (
    props.donnees.volumes
      .filter((v) => sous(v.path))
      .sort((a, b) => b.path.length - a.path.length)[0] ?? null
  )
})

function aller(chemin: string): void {
  void props.envoyer({ op: 'explore_local', path: chemin })
}

/**
 * Revient au choix du volume.
 *
 * Rouvrir l'assistant remet son état à zéro côté plugin : c'est exactement ce
 * qu'il faut, et cela évite d'inventer une opération pour le dire.
 */
function auxVolumes(): void {
  void props.envoyer({ op: 'explore_open', kind: 'local' })
}

function descendre(nom: string): void {
  // Le chemin se compose ici : l'arbre n'émet que des noms, parce qu'un chemin
  // local et un chemin SMB ne se composent pas de la même façon.
  aller(`${ex.value.path.replace(/\/$/, '')}/${nom}`)
}

function remonter(): void {
  const v = volumeCourant.value
  // Au sommet d'un volume, remonter ramène à la **liste des volumes** et non au
  // parent : on ne veut pas se retrouver à parcourir `/media` parce qu'on
  // cherchait une clé USB, ni surtout rester enfermé dans le premier volume
  // choisi sans pouvoir en essayer un autre.
  if (!v || ex.value.path === v.path) {
    auxVolumes()
    return
  }
  const parent = ex.value.path.replace(/\/[^/]+\/?$/, '')
  aller(parent.startsWith(v.path) && parent !== '' ? parent : v.path)
}

async function choisir(): Promise<void> {
  const ok = await props.envoyer({
    op: 'add_source',
    kind: 'local',
    path: ex.value.path,
    host: '',
    share: '',
    subpath: null,
    user: '',
    domain: '',
    password: '',
    writable: false,
  })
  if (ok) fermer()
}

function fermer(): void {
  void props.envoyer({ op: 'explore_close' })
  emit('fermer')
}
</script>

<template>
  <Dialog :open="ouvert" @update:open="(v: boolean) => !v && fermer()">
    <DialogContent data-dlg-appareil>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_device_title') }}</DialogTitle>
        <!-- Pas décorative : reka-ui la rattache par `aria-describedby`, et son
             absence laisse un lecteur d'écran annoncer une boîte de dialogue
             dont il ne sait rien dire. -->
        <DialogDescription>{{ t('dlg_device_desc') }}</DialogDescription>
      </DialogHeader>

      <div v-if="!dansLArbre" class="space-y-2">
        <p class="text-sm text-muted-foreground">{{ t('volumes_label') }}</p>
        <!-- Une liste vide sans phrase se lirait comme un chargement qui n'a
             pas fini. -->
        <p v-if="!donnees.volumes.length" class="text-sm text-muted-foreground" data-no-volumes>
          {{ t('no_volumes') }}
        </p>
        <button
          v-for="v in donnees.volumes"
          :key="v.path"
          type="button"
          data-volume
          class="flex w-full items-center gap-2 rounded px-2 py-1 text-left text-sm hover:bg-accent"
          :disabled="fige"
          @click="aller(v.path)"
        >
          <span class="flex-1 truncate">{{ v.path }}</span>
          <span class="text-xs text-muted-foreground">{{ v.fstype }}</span>
        </button>
      </div>

      <template v-else>
        <!-- Retour explicite au choix du volume : le seul autre chemin est de
             remonter jusqu'au sommet, ce qui ne se devine pas. -->
        <button
          type="button"
          data-aux-volumes
          class="self-start rounded px-2 py-1 text-left text-sm text-muted-foreground hover:bg-accent"
          :disabled="fige"
          @click="auxVolumes"
        >
          ← {{ t('volumes_label') }}
        </button>
        <ChoixDossier
          :exploration="ex"
          :t="t"
          :fige="fige"
          @descendre="descendre"
          @remonter="remonter"
        />
      </template>

      <!-- Le refus s'affiche **ici** et pas seulement sur la page : derrière le
           voile gris de la boîte de dialogue, le bandeau de la page est à peu
           près invisible au moment précis où il compte. -->
      <p v-if="message" class="text-sm text-destructive" data-dlg-message>{{ message }}</p>

      <div class="flex justify-end gap-2">
        <Button variant="ghost" data-annuler @click="fermer">{{ t('btn_cancel') }}</Button>
        <Button data-choisir :disabled="fige || !dansLArbre" @click="choisir">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
