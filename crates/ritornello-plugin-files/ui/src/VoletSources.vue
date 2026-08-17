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

    <!-- Le montage suit désormais la déclaration, sans que l'utilisateur ait un
         bouton à trouver. Sans ce rapport, une source resterait « non montée »
         sans jamais dire pourquoi — et le réessai n'aurait nulle part où
         vivre. -->
    <div v-if="donnees.mountError" class="space-y-1" data-mount-error>
      <p class="text-sm text-destructive">
        {{ t('mount_error_title') }} {{ donnees.mountError }}
      </p>
      <Button variant="outline" size="sm" data-retry-mount :disabled="fige" @click="remonter">
        {{ t('btn_retry_mount') }}
      </Button>
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
