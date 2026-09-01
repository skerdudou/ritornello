<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, createT, Input, Label, toast,
  type Catalog,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` is part of the plugin UI contract, just like `catalog`: the
// **absolute** prefix under which the core serves this plugin's routes
// (`/plugins/mpd/`), provided by the shell.
//
// **Required** prop, no default value: the name under which this plugin is
// served comes from `plugins.toml`, hence from the deployment, not from this
// file. A default of `/plugins/mpd/` would be wrong as soon as the operator
// declares this plugin under another name, and wrong *silently* — every
// request from this page would then go to a nonexistent plugin (404, page
// that looks dead). Better to require the shell to provide the prefix — which
// a `PluginView` test verifies, and which `contract.test.ts` verifies here on
// the module side.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** Absolute URL of one of this plugin's routes, built from `base`. */
function url(path: string): string {
  return `${props.base}${path}`
}

// Default values on the Rust side (`Config::default`): a coherent first render
// before the GET completes, rather than an empty field or a port at zero that
// would flash an invalid value on screen.
const listen = ref('0.0.0.0')
const port = ref(6600)

// Client-side guard, modelled exactly on `Config::validate` (non-empty address
// once whitespace is trimmed, port in 1..=65535): the only two refusals the
// server knows for these two fields. Since the rule is the same on both sides,
// a pair that passes this guard is by construction accepted by the server —
// see the comment on `save` below for what that implies for the 422 refusal
// test.
const listenInvalid = computed(() => !listen.value.trim())
const portInvalid = computed(() => {
  const p = Number(port.value)
  return !Number.isInteger(p) || p < 1 || p > 65535
})
const hasErrors = computed(() => listenInvalid.value || portInvalid.value)

async function reload(): Promise<void> {
  try {
    const data = await api.get<{ listen: string; port: number }>(url('api/data'))
    listen.value = data.listen
    port.value = data.port
  } catch (e) {
    // No catalog key covers this failure (the plugin always serves a
    // configuration, at worst the defaults): the raw request message is the
    // only text available, like the fallback GETs of `ConfigView.vue`.
    toast.error((e as Error).message)
  }
}

onMounted(reload)

/**
 * Saves the listen settings. The plugin rebinds itself to the new
 * address/port pair as soon as the save succeeds (see `session::listen`);
 * `restart_notice`, always visible above the form, says what remains true —
 * clients already connected keep their session on the old port. Nothing here
 * drives the rebinding: the page only persists the setting, and `admin.rs` is
 * what notifies the network half.
 *
 * `api.put` never rejects (network down included): the result is the only
 * source of truth, never an exception to catch. A refusal (422) already
 * carries the text translated on the server side (same convention as the
 * other plugins: `Config::validate`/`save` return a catalog key, which
 * `admin.rs` resolves through its own catalog before answering) — so this
 * page displays it as is, without retranslating it. The server remains the
 * sole judge: a value that passes `hasErrors` below may still be refused for
 * another reason (I/O, malformed request), and that path remains exactly this
 * one.
 */
async function save(): Promise<void> {
  // Belt and braces, like `RadioAdmin.save`: `:disabled` on the button is the
  // normal route, but does not protect against a synthetic click that would
  // bypass the button's visual state (developer tools, extension, a future
  // template refactor that would forget the binding).
  if (hasErrors.value) return
  const err = await api.put(url('api/data'), { listen: listen.value, port: Number(port.value) })
  toast[err ? 'error' : 'success'](err ?? t.value('saved'))
}
</script>

<template>
  <Card class="max-w-md">
    <CardHeader>
      <CardTitle>{{ t('title') }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p data-restart-notice class="text-sm text-muted-foreground">{{ t('restart_notice') }}</p>
      <div class="space-y-1">
        <Label for="mpd-listen">{{ t('listen_label') }}</Label>
        <Input id="mpd-listen" v-model="listen" data-listen :aria-invalid="listenInvalid" />
        <p v-if="listenInvalid" data-listen-error class="text-xs text-destructive">
          {{ t('listen_empty') }}
        </p>
      </div>
      <div class="space-y-1">
        <Label for="mpd-port">{{ t('port_label') }}</Label>
        <Input
          id="mpd-port" v-model="port" type="number" min="1" max="65535" data-port
          :aria-invalid="portInvalid"
        />
        <p v-if="portInvalid" data-port-error class="text-xs text-destructive">
          {{ t('port_zero') }}
        </p>
      </div>
      <Button data-save :disabled="hasErrors" @click="save">{{ t('btn_save') }}</Button>
    </CardContent>
  </Card>
</template>
