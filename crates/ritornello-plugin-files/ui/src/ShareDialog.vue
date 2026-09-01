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
 * "Network share" wizard.
 *
 * Three stages: host, then shares, then folders. Manual mode is not a
 * shameful fallback: it is there for when `smbclient` is missing, and
 * without it this effort would remove a capability that exists today.
 */
const props = defineProps<{
  data: Data
  t: T
  send: Send
  frozen: boolean
  open: boolean
  /**
   * Last refusal from the plugin, as the page received it.
   *
   * It lands here because a refusal born inside the dialog must display
   * there: the main page's banner is behind the dialog's grey veil, hence
   * about invisible right when it matters most.
   */
  message: string
}>()
const emit = defineEmits<{ close: [] }>()

const host = ref('')
const user = ref('')
const password = ref('')
const domain = ref('')
/**
 * Manual mode is **forced** when the wizard cannot work.
 *
 * Without `smbclient` there is nothing to browse: offering a toggle to a
 * dead wizard, and a greyed-out "Connect" button, gave two things to
 * understand for a choice that does not exist. The reason stays displayed
 * (`smb_unavailable`): it is what explains why the fields replace the
 * wizard.
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
 * Full address of the folder being browsed.
 *
 * `exploration.path` is relative to the share: displayed alone, it made the
 * share "vanish" as soon as it was entered, with no landmark saying which
 * one was being browsed.
 */
const fullPath = computed(() =>
  [`//${ex.value.host}/${ex.value.share}`, ex.value.path].filter(Boolean).join('/'),
)

// The `Dialog` stays **mounted** when closed: without this reset, the input
// of a previous opening would reappear at the next one — password included,
// which has no business staying in memory once the dialog is closed again.
watch(
  () => props.open,
  (open) => {
    if (!open) return
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

function chooseShare(name: string): void {
  void props.send({ op: 'smb_browse', share: name, path: '' })
}

function descend(name: string): void {
  const next = ex.value.path ? `${ex.value.path}/${name}` : name
  void props.send({ op: 'smb_browse', share: ex.value.share, path: next })
}

/**
 * Returns to the share list, without reconnecting.
 *
 * Fix for a reported defect: at the top of a share, goUp did nothing at
 * all, and there was no way to try another one without closing the dialog.
 * The operation is distinct from `smb_connect` because it must **not**
 * trigger a new network call: the shares are already known.
 */
function toShares(): void {
  void props.send({ op: 'smb_shares' })
}

function goUp(): void {
  // At the top of the share, goUp returns to the share list rather than
  // doing nothing.
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
  // In manual mode, everything comes from the fields; in wizard mode,
  // everything comes from what has been browsed — and the password stays
  // empty, because it already lives in the plugin's session and the page
  // never received it back.
  const payload = manual.value
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
  const ok = await props.send({ op: 'add_source', kind: 'smb', path: null, writable: false, ...payload })
  if (ok) close()
}

function close(): void {
  void props.send({ op: 'explore_close' })
  emit('close')
}
</script>

<template>
  <Dialog :open="open" @update:open="(v: boolean) => !v && close()">
    <DialogContent class="sm:max-w-2xl" data-share-dialog>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_share_title') }}</DialogTitle>
        <!-- Not decorative: reka-ui ties it in via `aria-describedby`, and
             its absence leaves a screen reader announcing a dialog it can
             say nothing about. -->
        <DialogDescription>{{ t('dlg_share_desc') }}</DialogDescription>
      </DialogHeader>

      <!-- Greyed out, never broken: that is the System tab's convention. A
           button that cannot work says why, instead of failing on click. -->
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
        <!-- Wider than the "share" field: its "(optional)" marker is what
             tells the two boxes apart, mistaken for two folders to supply
             as long as it was missing. Truncated, it would say nothing more. -->
        <Input
          v-model="manualSubpath"
          data-manual-subpath
          class="w-56"
          :placeholder="t('ph_subpath')"
        />
      </div>

      <template v-else>
        <p v-if="ex.error" class="min-w-0 break-words text-sm text-destructive" data-share-error>{{ ex.error }}</p>

        <!-- `min-w-0` for the same reason as in the local wizard: a grid
             child does not shrink below its content's width without
             permission, and a long share name would push the dialog past
             its own background. -->
        <div v-if="shareList" class="min-w-0 space-y-1">
          <p class="text-sm text-muted-foreground">{{ t('shares_label') }}</p>
          <button
            v-for="s in ex.shares"
            :key="s"
            type="button"
            data-share
            class="block w-full truncate rounded px-2 py-1 text-left text-sm hover:bg-accent"
            :disabled="frozen || ex.busy"
            :title="s"
            @click="chooseShare(s)"
          >
            {{ s }}
          </button>
        </div>

        <template v-else-if="inTree">
          <!-- Explicit return to the share list: without it, one stayed
               locked into the first share chosen. -->
          <button
            type="button"
            data-to-shares
            class="self-start rounded px-2 py-1 text-left text-sm text-muted-foreground hover:bg-accent"
            :disabled="frozen"
            @click="toShares"
          >
            ← {{ t('shares_label') }}
          </button>
          <FolderPicker
            :exploration="ex"
            :t="t"
            :frozen="frozen"
            :path="fullPath"
            @descend="descend"
            @goUp="goUp"
          />
        </template>
      </template>

      <!-- The refusal displays **here** and not only on the page: behind
           the dialog's grey veil, the page's banner is about invisible at
           the precise moment it matters. -->
      <p v-if="message" class="min-w-0 break-words text-sm text-destructive" data-dlg-message>{{ message }}</p>

      <div class="flex flex-wrap justify-end gap-2">
        <Button v-if="!manualForced" variant="ghost" data-manual @click="manualChosen = !manualChosen">
          {{ manual ? t('btn_assistant') : t('btn_manual') }}
        </Button>
        <Button variant="ghost" data-cancel @click="close">{{ t('btn_cancel') }}</Button>
        <Button
          v-if="!manual"
          variant="secondary"
          data-connect
          :disabled="frozen || ex.busy"
          @click="connect"
        >
          {{ ex.busy ? t('connecting') : t('btn_connect') }}
        </Button>
        <Button data-choose :disabled="frozen || (!manual && !inTree)" @click="choose">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
