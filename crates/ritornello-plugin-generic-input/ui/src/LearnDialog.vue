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

/**
 * Key-learning popup.
 *
 * Purely presentational component: it does not talk to the server, and
 * knows nothing of the probing mechanics or the 30 s timeout. It displays
 * what it is given and emits what it is asked to — the page that embeds it
 * alone keeps the truth (the `add` state, the probing, the cancellation on
 * the plugin side).
 */
const props = defineProps<{
  open: boolean
  /**
   * Translator, as `createT` renders it: the key, and the `{name}` tokens
   * to substitute into it. The full signature, not a reduced `(key) =>
   * string`: interpolation belongs to the translator (which replaces
   * **every** occurrence of a token), not to its callers.
   */
  t: (key: string, params?: Record<string, string | number>) => string
  /** **Already translated** label of the learned action, for the title. */
  action: string
  /** Device name, injected into the description. */
  device: string
  /** State of the "add" checkbox (v-model:add on the parent side). */
  add: boolean
  /**
   * Seconds remaining before abandonment, as computed by the page.
   *
   * The popup does not count them down itself: the deadline belongs to the
   * page, which already holds the probing and the cancellation on the
   * plugin side. A second timer here would drift from the first, and would
   * display a figure nothing guarantees.
   */
  seconds: number
}>()
const emit = defineEmits<{ cancel: []; 'update:add': [boolean] }>()

/**
 * Popup title: the dash only separates when there is an action to name.
 *
 * The page resets its learned row to `null` — hence `action` to the empty
 * string — as soon as the closing gesture happens, while reka-ui's
 * `Presence` keeps the content mounted for the duration of the exit fade
 * (`duration-200`). Without this guard, the title would display "Learning
 * a key —" throughout the closing animation.
 */
const title = computed(() =>
  props.action ? `${props.t('dlg_learn_title')} — ${props.action}` : props.t('dlg_learn_title'),
)
</script>

<template>
  <!-- Escape, the click on the overlay, and the close icon that
       `DialogContent` places by default all close the Dialog (`update:open`
       to `false`): we route them through `cancel`, exactly like the button,
       so only a single cancellation path exists. -->
  <Dialog :open="props.open" @update:open="(v: boolean) => !v && emit('cancel')">
    <DialogContent data-dlg-learn>
      <DialogHeader>
        <DialogTitle>{{ title }}</DialogTitle>
        <!-- Not decorative: reka-ui attaches it via `aria-describedby`, and
             its absence leaves a screen reader announcing a dialog box it
             can say nothing about. -->
        <DialogDescription>
          {{ props.t('dlg_learn_desc', { device: props.device }) }}
        </DialogDescription>
      </DialogHeader>

      <!-- At zero, nothing more: the page has already stopped learning, and
           an "0 s left" displayed during the closing fade would be a
           countdown that lies. -->
      <p
        v-if="props.seconds > 0"
        data-learn-countdown
        class="text-sm text-muted-foreground"
        aria-live="polite"
      >
        {{ props.t('learn_countdown', { s: props.seconds }) }}
      </p>

      <label class="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          data-learn-append
          :checked="props.add"
          @change="emit('update:add', ($event.target as HTMLInputElement).checked)"
        />
        {{ props.t('learn_append_label') }}
      </label>

      <div class="flex justify-end">
        <Button variant="secondary" data-learn-cancel @click="emit('cancel')">
          {{ props.t('btn_cancel') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
