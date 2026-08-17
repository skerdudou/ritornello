<script setup lang="ts">
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Input,
} from '@ritornello/ui'
import { computed, ref } from 'vue'
import ChoixDossier from './ChoixDossier.vue'
import type { Donnees, Envoyer, T } from './donnees'

/**
 * Assistant « partage réseau ».
 *
 * Trois temps : hôte, puis partages, puis dossiers. Le mode manuel n'est pas un
 * repli honteux : il sert quand `smbclient` manque, et sans lui ce chantier
 * retirerait une capacité qui existe aujourd'hui.
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

const host = ref('')
const user = ref('')
const password = ref('')
const domain = ref('')
const manuel = ref(false)
const partageManuel = ref('')
const sousCheminManuel = ref('')

const ex = computed(() => props.donnees.explore)
const dansLArbre = computed(() => ex.value.kind === 'smb' && ex.value.share !== '')
const listeDePartages = computed(() => ex.value.shares.length > 0 && !dansLArbre.value)

function connecter(): void {
  void props.envoyer({
    op: 'smb_connect',
    host: host.value.trim(),
    user: user.value.trim(),
    password: password.value,
    domain: domain.value.trim(),
  })
}

function choisirPartage(nom: string): void {
  void props.envoyer({ op: 'smb_browse', share: nom, path: '' })
}

function descendre(nom: string): void {
  const suite = ex.value.path ? `${ex.value.path}/${nom}` : nom
  void props.envoyer({ op: 'smb_browse', share: ex.value.share, path: suite })
}

function remonter(): void {
  void props.envoyer({
    op: 'smb_browse',
    share: ex.value.share,
    path: ex.value.path.replace(/\/?[^/]+$/, ''),
  })
}

async function choisir(): Promise<void> {
  // En mode manuel, tout vient des champs ; en mode assistant, tout vient de
  // ce qu'on a parcouru — et le mot de passe reste vide, parce qu'il vit déjà
  // dans la session du plugin et que la page ne l'a jamais reçu en retour.
  const charge = manuel.value
    ? {
        host: host.value.trim(),
        share: partageManuel.value.trim(),
        subpath: sousCheminManuel.value.trim() || null,
        user: user.value.trim(),
        domain: domain.value.trim(),
        password: password.value,
      }
    : {
        host: ex.value.host,
        share: ex.value.share,
        subpath: ex.value.path || null,
        user: '',
        domain: '',
        password: '',
      }
  const ok = await props.envoyer({ op: 'add_source', kind: 'smb', path: null, writable: false, ...charge })
  if (ok) fermer()
}

function fermer(): void {
  void props.envoyer({ op: 'explore_close' })
  emit('fermer')
}
</script>

<template>
  <Dialog :open="ouvert" @update:open="(v: boolean) => !v && fermer()">
    <DialogContent data-dlg-partage>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_share_title') }}</DialogTitle>
        <!-- Pas décorative : reka-ui la rattache par `aria-describedby`, et son
             absence laisse un lecteur d'écran annoncer une boîte de dialogue
             dont il ne sait rien dire. -->
        <DialogDescription>{{ t('dlg_share_desc') }}</DialogDescription>
      </DialogHeader>

      <!-- Grisé, jamais planté : c'est la convention de l'onglet Système. Le
           bouton qui ne peut pas marcher dit pourquoi, au lieu d'échouer au
           clic. -->
      <p
        v-if="!donnees.canBrowseSmb"
        class="text-sm text-muted-foreground"
        data-smb-unavailable
      >
        {{ t('smb_unavailable') }}
      </p>

      <div class="flex flex-wrap gap-2">
        <Input v-model="host" data-host class="w-44" :placeholder="t('ph_host')" />
        <Input v-model="user" data-user class="w-32" :placeholder="t('ph_user')" />
        <Input
          v-model="password"
          type="password"
          data-password
          class="w-32"
          :placeholder="t('ph_password')"
        />
        <Input v-model="domain" data-domain class="w-28" :placeholder="t('ph_domain')" />
      </div>

      <div v-if="manuel" class="flex flex-wrap gap-2">
        <Input
          v-model="partageManuel"
          data-manual-share
          class="w-40"
          :placeholder="t('ph_share')"
        />
        <Input
          v-model="sousCheminManuel"
          data-manual-subpath
          class="w-40"
          :placeholder="t('ph_subpath')"
        />
      </div>

      <template v-else>
        <p v-if="ex.error" class="min-w-0 break-words text-sm text-destructive" data-partage-erreur>{{ ex.error }}</p>

        <!-- `min-w-0` pour la même raison que dans l'assistant local : un enfant
             de grille ne rétrécit pas sous la largeur de son contenu sans
             autorisation, et un nom de partage long pousserait la popin hors de
             son propre fond. -->
        <div v-if="listeDePartages" class="min-w-0 space-y-1">
          <p class="text-sm text-muted-foreground">{{ t('shares_label') }}</p>
          <button
            v-for="s in ex.shares"
            :key="s"
            type="button"
            data-share
            class="block w-full truncate rounded px-2 py-1 text-left text-sm hover:bg-accent"
            :disabled="fige || ex.busy"
            :title="s"
            @click="choisirPartage(s)"
          >
            {{ s }}
          </button>
        </div>

        <ChoixDossier
          v-else-if="dansLArbre"
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
      <p v-if="message" class="min-w-0 break-words text-sm text-destructive" data-dlg-message>{{ message }}</p>

      <div class="flex flex-wrap justify-end gap-2">
        <Button variant="ghost" data-manuel @click="manuel = !manuel">
          {{ manuel ? t('btn_assistant') : t('btn_manual') }}
        </Button>
        <Button variant="ghost" data-annuler @click="fermer">{{ t('btn_cancel') }}</Button>
        <Button
          v-if="!manuel"
          variant="secondary"
          data-connect
          :disabled="fige || !donnees.canBrowseSmb || ex.busy"
          @click="connecter"
        >
          {{ ex.busy ? t('connecting') : t('btn_connect') }}
        </Button>
        <Button data-choisir :disabled="fige || (!manuel && !dansLArbre)" @click="choisir">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
