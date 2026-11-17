<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@ritornello/ui'
import { ref } from 'vue'
import type { T } from './data'

defineProps<{
  t: T
  open: boolean
  /** A probe is being sent. */
  busy: boolean
  /** The plugin's refusal of the last submission: the dialog stays open on it. */
  error: string
  /** The page could not read the table: nothing may be sent. */
  frozen: boolean
}>()

const emit = defineEmits<{
  'update:open': [open: boolean]
  submit: [text: string]
}>()

// The page closes the dialog and remounts it (a new key) once the plugin has
// accepted the links, which is what empties the field; a refusal leaves what
// was pasted in place, to be corrected.
const text = ref('')
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{{ t('dlg_add_title') }}</DialogTitle>
        <DialogDescription>{{ t('add_placeholder') }}</DialogDescription>
      </DialogHeader>
      <textarea
        v-model="text"
        data-links
        rows="6"
        class="border-input bg-transparent placeholder:text-muted-foreground w-full min-w-0 rounded-md border px-3 py-2 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-3"
        :placeholder="t('add_placeholder')"
      />
      <p v-if="error" class="text-sm text-destructive" role="alert" data-add-error>{{ error }}</p>
      <div class="flex justify-end gap-2">
        <Button variant="ghost" data-add-cancel @click="emit('update:open', false)">{{ t('btn_cancel') }}</Button>
        <Button data-add-submit :disabled="frozen || busy || !text.trim()" @click="emit('submit', text)">
          {{ t('btn_add_submit') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
