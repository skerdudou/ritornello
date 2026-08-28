<script setup lang="ts">
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, createT, Input, Label, toast,
  type Catalog,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` fait partie du contract des IHM de plugin, au meme titre que
// `catalog` : le prefixe **absolu** sous lequel le coeur sert les routes de ce
// plugin (`/plugins/mpd/`), fourni par le shell.
//
// Prop **requise**, sans valeur par defaut : le nom sous lequel ce plugin est
// servi vient de `plugins.toml`, donc du deploiement, et non de ce fichier. Un
// defaut `/plugins/mpd/` serait faux des que l'operateur declare ce plugin
// sous un autre nom, et le serait *silencieusement* — toutes les requetes de
// cette page partiraient alors vers un plugin inexistant (404, page qui
// semble morte). Mieux vaut que le shell soit tenu de fournir le prefixe — ce
// qu'un test de `PluginView` verifie, et que `contract.test.ts` verifie ici
// cote module.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

/** URL absolue d'une route de ce plugin, construite depuis `base`. */
function url(chemin: string): string {
  return `${props.base}${chemin}`
}

// Valeurs par defaut du cote Rust (`Config::default`) : un premier rendu
// coherent avant que le GET n'aboutisse, plutot qu'un champ vide ou un port a
// zero qui flasherait une valeur invalide a l'ecran.
const listen = ref('0.0.0.0')
const port = ref(6600)

// Garde cote client, calquee exactement sur `Config::valider` (adresse non
// vide une fois retiree des espaces, port dans 1..=65535) : les deux seuls
// refus que le serveur connait pour ces deux champs. Comme la regle est la
// meme des deux cotes, un couple qui passe ce garde est par construction
// accepte par le serveur — voir le commentaire d'`save` plus bas sur
// ce que cela implique pour le test du refus 422.
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
    // Aucune cle de catalogue ne couvre cet echec (le greffon sert toujours
    // une configuration, au pire les defauts) : le message brut de la
    // requete est le seul texte disponible, comme les GET de secours de
    // `ConfigView.vue`.
    toast.error((e as Error).message)
  }
}

onMounted(reload)

/**
 * Enregistre l'ecoute. Le greffon se relie de lui-meme au nouveau couple
 * adresse/port des que l'enregistrement aboutit (voir `session::ecouter`) ;
 * `restart_notice`, toujours visible au-dessus du formulaire, dit ce qui reste
 * vrai — les clients deja connectes gardent leur session sur l'ancien port.
 * Rien ici ne pilote la reliaison : la page ne fait que persister le reglage,
 * et c'est `admin.rs` qui previent la moitie reseau.
 *
 * `api.put` ne rejette jamais (reseau coupe compris) : le resultat est la
 * seule source de verite, jamais une exception a rattraper. Un refus (422)
 * porte deja le texte traduit du cote serveur (meme convention que les
 * autres greffons : `Config::valider`/`save` renvoient une cle de
 * catalogue, que `admin.rs` resout via son propre catalogue avant de
 * repondre) — cette page l'affiche donc tel quel, sans le retraduire. Le
 * serveur reste seul juge : une valeur qui passe `hasErrors` ci-dessous
 * peut encore etre refusee pour une autre raison (E/S, requete malformee),
 * et ce chemin-la reste exactement celui-ci.
 */
async function save(): Promise<void> {
  // Ceinture et bretelles, comme `RadioAdmin.save` : `:disabled` sur
  // le bouton est la voie normale, mais ne protege pas un clic synthetique
  // qui contournerait l'etat visuel du bouton (outils de developpement,
  // extension, futur refactor du gabarit qui oublierait la liaison).
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
