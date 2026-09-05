<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, createT,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, toast,
  type Catalog,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` is part of the plugin UI contract, just like `catalog`: the
// **absolute** prefix under which the core serves this plugin's routes
// (`/plugins/cd/`), provided by the shell.
//
// **Required** prop, no default value: the name under which this plugin is
// served comes from `plugins.toml`, hence from the deployment, not from this
// file. A default of `/plugins/cd/` would be wrong as soon as the operator
// declares this plugin under another name, and wrong *silently* — every
// request from this page would then go to a nonexistent plugin.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** Absolute URL of one of this plugin's routes, built from `base`. */
function url(path: string): string {
  return `${props.base}${path}`
}

/**
 * The three values the plugin accepts, in the order they escalate: do
 * nothing, start, resume. Kept as a list rather than three `SelectItem`s
 * written out so that the labels and the values cannot drift apart — and so
 * the set is stated in exactly one place, mirroring `state::OnArrival`.
 */
const CHOICES = ['nothing', 'first_track', 'last_track'] as const
type Choice = (typeof CHOICES)[number]

/**
 * Default matching `OnArrival::default` on the Rust side: a coherent first
 * render before the GET completes, rather than an empty select that would
 * flash a value nobody chose.
 */
const onArrival = ref<Choice>('nothing')

/**
 * Label of a choice.
 *
 * An exhaustive switch with **literal** keys rather than a
 * `t(\`arrival_${choice}\`)` one-liner, and that is not verbosity for its own
 * sake: `i18nKeysUsed.test.ts` collects the keys called with a literal, so a
 * key built by interpolation escapes the net that exists precisely to stop a
 * missing key from reaching the screen as raw text. The switch also makes TS
 * refuse a new choice that forgets its label.
 */
function label(choice: Choice): string {
  switch (choice) {
    case 'nothing':
      return t.value('arrival_nothing')
    case 'first_track':
      return t.value('arrival_first_track')
    case 'last_track':
      return t.value('arrival_last_track')
  }
}

async function reload(): Promise<void> {
  try {
    const data = await api.get<{ on_arrival: Choice }>(url('api/data'))
    // Guarded rather than assigned blindly: an unknown value — an older
    // plugin, a hand-edited state file — would leave the `Select` pointing at
    // a value with no matching `SelectItem`, i.e. a blank control. Falling
    // back on the default shows what the plugin actually does in that case.
    onArrival.value = CHOICES.includes(data.on_arrival) ? data.on_arrival : 'nothing'
  } catch (e) {
    // No catalog key covers this failure (the plugin always serves a setting,
    // at worst the default): the raw request message is the only text
    // available, like the fallback GETs of `ConfigView.vue`.
    toast.error((e as Error).message)
  }
}

onMounted(reload)

/**
 * Saves the setting. It applies from the next arrival on — the Source half
 * reads the shared value at each `Activate`/`Wake` rather than caching it —
 * so there is nothing to restart.
 *
 * `api.put` never rejects (network down included): the result is the only
 * source of truth, never an exception to catch. A refusal already carries the
 * text translated on the server side, so this page displays it as is without
 * retranslating it.
 */
async function save(): Promise<void> {
  const err = await api.put(url('api/data'), { on_arrival: onArrival.value })
  toast[err ? 'error' : 'success'](err ?? t.value('saved'))
}
</script>

<template>
  <Card class="max-w-md">
    <CardHeader>
      <CardTitle>{{ t('title') }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="space-y-1">
        <!-- The neighbouring <label> is not associated with the trigger (no
             for/id through the Select component): the aria-label carries the
             accessible name. Same arrangement as `InputAdmin.vue`. -->
        <label class="text-sm font-medium">{{ t('arrival_label') }}</label>
        <Select v-model="onArrival">
          <SelectTrigger data-arrival class="w-full" :aria-label="t('arrival_label')">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem v-for="choice in CHOICES" :key="choice" :value="choice">
              {{ label(choice) }}
            </SelectItem>
          </SelectContent>
        </Select>
        <p data-arrival-help class="text-xs text-muted-foreground">{{ t('arrival_help') }}</p>
        <!-- Shown only for the resume: it is the only choice whose behaviour
             depends on which disc is in the tray, and stating that only when
             it applies keeps the page from explaining a rule that is not in
             force. -->
        <p
          v-if="onArrival === 'last_track'"
          data-resume-help
          class="text-xs text-muted-foreground"
        >
          {{ t('arrival_last_track_help') }}
        </p>
      </div>
      <Button data-save @click="save">{{ t('btn_save') }}</Button>
    </CardContent>
  </Card>
</template>
