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
import { computed, ref, watch } from 'vue'
import FolderPicker from './FolderPicker.vue'
import type { Data, Send, T } from './data'

/**
 * Assistant « partage réseau ».
 *
 * Trois temps : hôte, puis partages, puis dossiers. Le mode manual n'est pas un
 * repli honteux : il sert quand `smbclient` manque, et sans lui ce chantier
 * retirerait une capacité qui existe aujourd'hui.
 */
const props = defineProps<{
  data: Data
  t: T
  send: Send
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
const emit = defineEmits<{ close: [] }>()

const host = ref('')
const user = ref('')
const password = ref('')
const domain = ref('')
/**
 * Le mode manual est **imposé** quand l'assistant ne peut pas fonctionner.
 *
 * Sans `smbclient` il n'y a rien à parcourir : offrir une bascule vers un
 * assistant inerte, et un bouton « Se connect » grisé, donnait deux
 * commandes à comprendre pour un choix qui n'existe pas. La raison reste
 * affichée (`smb_unavailable`) : c'est elle qui explique pourquoi les champs
 * remplacent l'assistant.
 */
const manualForced = computed(() => !props.data.canBrowseSmb)
const manualChosen = ref(false)
const manual = computed(() => manualForced.value || manualChosen.value)
const manualShare = ref('')
const manualSubpath = ref('')

const ex = computed(() => props.data.explore)
const inTree = computed(() => ex.value.kind === 'smb' && ex.value.share !== '')
const shareList = computed(() => ex.value.shares.length > 0 && !inTree.value)

/**
 * Adresse complète du dossier parcouru.
 *
 * `exploration.path` est relatif au partage : affiché seul, il faisait
 * « disparaître » le partage dès qu'on entrait dedans, sans qu'aucun repère ne
 * dise dans lequel on se trouvait.
 */
const fullPath = computed(() =>
  [`//${ex.value.host}/${ex.value.share}`, ex.value.path].filter(Boolean).join('/'),
)

// Le `Dialog` reste **monté** quand il est fermé : sans cette remise à zéro, la
// input d'une ouverture précédente réapparaîtrait à la suivante — mot de passe
// compris, ce qui n'a rien à faire en mémoire une fois la popin refermée.
watch(
  () => props.ouvert,
  (ouvert) => {
    if (!ouvert) return
    host.value = ''
    user.value = ''
    password.value = ''
    domain.value = ''
    manualChosen.value = false
    manualShare.value = ''
    manualSubpath.value = ''
  },
)

function connect(): void {
  void props.send({
    op: 'smb_connect',
    host: host.value.trim(),
    user: user.value.trim(),
    password: password.value,
    domain: domain.value.trim(),
  })
}

function chooseShare(nom: string): void {
  void props.send({ op: 'smb_browse', share: nom, path: '' })
}

function descend(nom: string): void {
  const suite = ex.value.path ? `${ex.value.path}/${nom}` : nom
  void props.send({ op: 'smb_browse', share: ex.value.share, path: suite })
}

/**
 * Revient à la liste des partages, sans se reconnecter.
 *
 * Correctif d'un défaut signalé : au sommet d'un partage, goUp ne faisait
 * rien du tout, et il n'existait aucun moyen d'en essayer un autre sans close
 * la popin. L'opération est distincte de `smb_connect` parce qu'elle ne doit
 * **pas** relancer un appel réseau : les partages sont déjà connus.
 */
function toShares(): void {
  void props.send({ op: 'smb_shares' })
}

function goUp(): void {
  // Au sommet du partage, goUp ramène à la liste des partages plutôt que de
  // ne rien faire.
  if (!ex.value.path) {
    toShares()
    return
  }
  void props.send({
    op: 'smb_browse',
    share: ex.value.share,
    path: ex.value.path.replace(/\/?[^/]+$/, ''),
  })
}

async function choose(): Promise<void> {
  // En mode manual, tout vient des champs ; en mode assistant, tout vient de
  // ce qu'on a parcouru — et le mot de passe reste vide, parce qu'il vit déjà
  // dans la session du plugin et que la page ne l'a jamais reçu en retour.
  const charge = manual.value
    ? {
        host: host.value.trim(),
        share: manualShare.value.trim(),
        subpath: manualSubpath.value.trim() || null,
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
  const ok = await props.send({ op: 'add_source', kind: 'smb', path: null, writable: false, ...charge })
  if (ok) close()
}

function close(): void {
  void props.send({ op: 'explore_close' })
  emit('close')
}
</script>

<template>
  <Dialog :open="ouvert" @update:open="(v: boolean) => !v && close()">
    <DialogContent class="sm:max-w-2xl" data-dlg-partage>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_share_title') }}</DialogTitle>
        <!-- Pas décorative : reka-ui la rattache par `aria-describedby`, et son
             absence laisse un player d'écran annoncer une boîte de dialogue
             dont il ne sait rien dire. -->
        <DialogDescription>{{ t('dlg_share_desc') }}</DialogDescription>
      </DialogHeader>

      <!-- Grisé, jamais planté : c'est la convention de l'onglet Système. Le
           bouton qui ne peut pas marcher dit pourquoi, au lieu d'échouer au
           clic. -->
      <p
        v-if="!data.canBrowseSmb"
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

      <div v-if="manual" class="flex flex-wrap gap-2">
        <Input
          v-model="manualShare"
          data-manual-share
          class="w-40"
          :placeholder="t('ph_share')"
        />
        <!-- Plus large que le champ « partage » : son marqueur « (optionnel) »
             est ce qui distingue les deux cases, prises pour deux dossiers à
             fournir tant qu'il manquait. Tronqué, il ne dirait plus rien. -->
        <Input
          v-model="manualSubpath"
          data-manual-subpath
          class="w-56"
          :placeholder="t('ph_subpath')"
        />
      </div>

      <template v-else>
        <p v-if="ex.error" class="min-w-0 break-words text-sm text-destructive" data-partage-erreur>{{ ex.error }}</p>

        <!-- `min-w-0` pour la même raison que dans l'assistant local : un enfant
             de grille ne rétrécit pas sous la largeur de son contenu sans
             autorisation, et un nom de partage long pousserait la popin hors de
             son propre fond. -->
        <div v-if="shareList" class="min-w-0 space-y-1">
          <p class="text-sm text-muted-foreground">{{ t('shares_label') }}</p>
          <button
            v-for="s in ex.shares"
            :key="s"
            type="button"
            data-share
            class="block w-full truncate rounded px-2 py-1 text-left text-sm hover:bg-accent"
            :disabled="fige || ex.busy"
            :title="s"
            @click="chooseShare(s)"
          >
            {{ s }}
          </button>
        </div>

        <template v-else-if="inTree">
          <!-- Retour explicite à la liste des partages : sans lui, on restait
               enfermé dans le premier partage choisi. -->
          <button
            type="button"
            data-aux-partages
            class="self-start rounded px-2 py-1 text-left text-sm text-muted-foreground hover:bg-accent"
            :disabled="fige"
            @click="toShares"
          >
            ← {{ t('shares_label') }}
          </button>
          <FolderPicker
            :exploration="ex"
            :t="t"
            :fige="fige"
            :path="fullPath"
            @descend="descend"
            @goUp="goUp"
          />
        </template>
      </template>

      <!-- Le refus s'affiche **ici** et pas seulement sur la page : derrière le
           voile gris de la boîte de dialogue, le bandeau de la page est à peu
           près invisible au moment précis où il compte. -->
      <p v-if="message" class="min-w-0 break-words text-sm text-destructive" data-dlg-message>{{ message }}</p>

      <div class="flex flex-wrap justify-end gap-2">
        <Button v-if="!manualForced" variant="ghost" data-manual @click="manualChosen = !manualChosen">
          {{ manual ? t('btn_assistant') : t('btn_manual') }}
        </Button>
        <Button variant="ghost" data-annuler @click="close">{{ t('btn_cancel') }}</Button>
        <Button
          v-if="!manual"
          variant="secondary"
          data-connect
          :disabled="fige || ex.busy"
          @click="connect"
        >
          {{ ex.busy ? t('connecting') : t('btn_connect') }}
        </Button>
        <Button data-choose :disabled="fige || (!manual && !inTree)" @click="choose">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
