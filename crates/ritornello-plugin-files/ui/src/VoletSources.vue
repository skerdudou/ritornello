<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ref } from 'vue'
import DialogueAppareil from './DialogueAppareil.vue'
import DialoguePartage from './DialoguePartage.vue'
import { cibleRacine, type Donnees, type Envoyer, type T } from './donnees'

/**
 * Les sources déclarées.
 *
 * Plus de formulaire de saisie : on ne tape plus une adresse à l'aveugle, on
 * parcourt puis on déclare. Ce volet se réduit donc à énumérer ce qui existe,
 * offrir les trois gestes qui portent sur une source, et ouvrir l'un des deux
 * assistants.
 */
const props = defineProps<{
  donnees: Donnees
  t: T
  envoyer: Envoyer
  fige: boolean
  /** Dernier refus du plugin, relayé aux popins pour qu'il s'y affiche. */
  message: string
}>()

const appareilOuvert = ref(false)
const partageOuvert = ref(false)

function ouvrir(kind: 'local' | 'smb'): void {
  // L'ouverture passe par le plugin, et pas seulement par un booléen local :
  // c'est lui qui porte l'état de l'assistant (volumes parcourus, session
  // ouverte), et une popin qui s'afficherait sans le prévenir hériterait de
  // l'état de la précédente.
  void props.envoyer({ op: 'explore_open', kind })
  if (kind === 'local') appareilOuvert.value = true
  else partageOuvert.value = true
}

function toutAjouter(nom: string): void {
  // Chemin vide = la source entière. Récursif et asynchrone côté plugin : la
  // réponse n'attend pas la fin du balayage, c'est le sondage de la page qui
  // en montre l'avancement.
  void props.envoyer({ op: 'add_dir', root: nom, path: '' })
}

function retirer(nom: string): void {
  void props.envoyer({ op: 'remove_source', name: nom })
}

function basculer(nom: string, writable: boolean): void {
  void props.envoyer({ op: 'set_writable', name: nom, writable })
}

function remonter(): void {
  void props.envoyer({ op: 'mount' })
}
</script>

<template>
  <section class="space-y-4" data-volet-sources>
    <h2 class="font-medium">{{ t('sources_title') }}</h2>

    <p v-if="!donnees.roots.length" class="text-sm text-muted-foreground" data-no-sources>
      {{ t('no_sources') }}
    </p>

    <div
      v-for="r in donnees.roots"
      :key="r.name"
      data-source-row
      class="flex flex-wrap items-center gap-2 rounded-md border border-border p-3"
    >
      <span class="text-xs text-muted-foreground" data-source-kind>
        {{ r.kind === 'local' ? t('kind_local') : t('kind_smb') }}
      </span>
      <span class="min-w-48 flex-1 truncate text-sm" data-source-target>{{ cibleRacine(r) }}</span>

      <!-- L'état du montage est **observé**, jamais saisi : il vient du plugin,
           qui lit /proc/mounts. -->
      <span v-if="r.kind === 'smb'" class="text-xs" data-source-mounted>
        {{ r.mounted ? t('mounted_yes') : t('mounted_no') }}
      </span>

      <!-- Le réessai suit l'état **observé**, et non le souvenir de la dernière
           tentative : `mount_error` vit en mémoire du plugin et repart vide à
           chaque redémarrage, si bien qu'on retrouvait « non monté » sans plus
           rien pour y remédier. `mounted`, lui, est lu dans /proc/mounts et dit
           toujours la vérité.
           Et il vit sur la ligne de la source, là où le problème s'affiche. -->
      <Button
        v-if="r.kind === 'smb' && !r.mounted"
        variant="outline"
        size="sm"
        data-retry-mount
        :disabled="fige"
        @click="remonter"
      >
        {{ t('btn_retry_mount') }}
      </Button>

      <label v-if="r.kind === 'smb'" class="flex items-center gap-1 text-sm">
        <input
          type="checkbox"
          data-writable
          :checked="r.writable"
          :disabled="fige"
          @change="basculer(r.name, ($event.target as HTMLInputElement).checked)"
        />
        {{ t('writable_label') }}
      </label>

      <Button
        variant="secondary"
        size="sm"
        data-add-all
        :disabled="fige"
        @click="toutAjouter(r.name)"
      >
        {{ t('btn_add_to_playlist') }}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        data-remove-source
        :aria-label="t('btn_remove_source')"
        :disabled="fige"
        @click="retirer(r.name)"
      >
        ✕
      </Button>
    </div>

    <!-- Le montage suit la déclaration, sans que l'utilisateur ait un bouton à
         trouver. Sans ce rapport, une source resterait « non montée » sans
         jamais dire pourquoi. Le texte seul : le réessai vit sur la ligne
         concernée, et deux boutons pour le même geste feraient hésiter. -->
    <p
      v-if="donnees.mountError"
      class="min-w-0 break-words text-sm text-destructive"
      data-mount-error
    >
      {{ t('mount_error_title') }} {{ donnees.mountError }}
    </p>

    <!-- Un montage qui ne répond plus n'est **pas** un échec de montage : il est
         monté, il se tait. Deux causes distinctes, donc deux messages distincts
         — les confondre enverrait réessayer un montage qui a réussi. C'est aussi
         ce qui explique les états « indéterminé » de la liste et les durées qui
         n'arrivent pas ; sans ce bloc, l'utilisateur n'a aucune cause à quoi les
         rattacher. -->
    <div
      v-if="donnees.unresponsive.length"
      class="min-w-0 rounded border border-destructive/50 p-2 text-sm"
      data-unresponsive
    >
      <p class="text-destructive">{{ t('unresponsive_title') }}</p>
      <ul class="ml-4 list-disc break-all font-mono text-xs">
        <li v-for="m in donnees.unresponsive" :key="m">{{ m }}</li>
      </ul>
      <p class="mt-1 text-muted-foreground">{{ t('unresponsive_hint') }}</p>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <Button variant="secondary" data-add-device :disabled="fige" @click="ouvrir('local')">
        {{ t('btn_add_device') }}
      </Button>
      <Button variant="secondary" data-add-share :disabled="fige" @click="ouvrir('smb')">
        {{ t('btn_add_share') }}
      </Button>
    </div>

    <DialogueAppareil
      :donnees="donnees"
      :t="t"
      :envoyer="envoyer"
      :fige="fige"
      :message="message"
      :ouvert="appareilOuvert"
      @fermer="appareilOuvert = false"
    />
    <DialoguePartage
      :donnees="donnees"
      :t="t"
      :envoyer="envoyer"
      :fige="fige"
      :message="message"
      :ouvert="partageOuvert"
      @fermer="partageOuvert = false"
    />
  </section>
</template>
