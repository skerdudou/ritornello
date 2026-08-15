<script setup lang="ts">
import { Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { cibleRacine, type Donnees, type Envoyer, type Racine, type T } from './donnees'

const props = defineProps<{ donnees: Donnees; t: T; envoyer: Envoyer; fige: boolean }>()

/**
 * Ligne en cours d'édition.
 *
 * `cle` est **côté navigateur uniquement** et sert au `:key` de la boucle : le
 * nom, lui, se saisit — s'en servir comme identité ferait recréer le champ à
 * chaque frappe, et le focus sauterait au premier caractère tapé.
 *
 * `password` n'est jamais rendu par `api/data` (le plugin ne ressort pas les
 * secrets) : il part donc toujours d'une chaîne vide, et une chaîne vide veut
 * dire « garde celui déjà enregistré » — pas « efface-le ».
 */
interface Ligne extends Racine {
  cle: number
  password: string
}

let prochaineCle = 0
function ligne(r: Racine): Ligne {
  prochaineCle += 1
  return { ...r, cle: prochaineCle, password: '' }
}

const lignes = ref<Ligne[]>([])
// Empreinte de ce que le serveur rendait au dernier remplissage du formulaire.
//
// Elle existe à cause du sondage : pendant un balayage, la page redemande
// `api/data` **chaque seconde**, et une resynchronisation naïve écraserait à
// chaque réponse la saisie en cours — l'utilisateur verrait le mot de passe
// qu'il tape disparaître sous ses doigts. On ne réécrit donc le formulaire que
// lorsque la liste rendue par le serveur a réellement changé.
let empreinteServeur = ''

function resynchroniser(): void {
  lignes.value = props.donnees.roots.map(ligne)
  empreinteServeur = JSON.stringify(props.donnees.roots)
}

watch(
  () => props.donnees.roots,
  (roots) => {
    if (JSON.stringify(roots) === empreinteServeur) return
    resynchroniser()
  },
  { immediate: true, deep: true },
)

function racineVide(kind: 'local' | 'smb'): Ligne {
  return ligne({
    name: '',
    kind,
    path: '',
    host: '',
    share: '',
    subpath: '',
    user: '',
    domain: '',
    writable: false,
    mounted: false,
  })
}

function ajouter(kind: 'local' | 'smb'): void {
  lignes.value.push(racineVide(kind))
}

function retirer(i: number): void {
  lignes.value.splice(i, 1)
}

/** Au moins un partage déclaré : c'est ce qui justifie d'expliquer le mot de passe. */
const aUnPartage = computed(() => lignes.value.some((l) => l.kind === 'smb'))

async function enregistrer(): Promise<void> {
  // `path` et `subpath` partent à `null` quand ils sont vides, et non à `""` :
  // côté plugin ce sont des `Option<String>`, et `Some("")` n'est pas « pas de
  // sous-chemin » — c'est un sous-chemin vide, que `Roots::validate` refuse
  // (`champ_sur` rejette la chaîne vide). Le partage serait alors impossible à
  // enregistrer sans qu'aucun champ visible n'ait l'air fautif. Les autres
  // champs partent toujours, même vides : ce sont des `String` côté plugin, et
  // les omettre l'obligerait à distinguer « absent » de « vidé ».
  const facultatif = (v: string) => (v.trim() ? v.trim() : null)
  const roots = lignes.value.map((l) => ({
    name: l.name.trim(),
    kind: l.kind,
    path: facultatif(l.path),
    host: l.host.trim(),
    share: l.share.trim(),
    subpath: facultatif(l.subpath),
    user: l.user.trim(),
    domain: l.domain.trim(),
    writable: l.writable,
    password: l.password,
  }))
  if (await props.envoyer({ op: 'save_roots', roots })) resynchroniser()
}

function monter(): void {
  void props.envoyer({ op: 'mount' })
}
</script>

<template>
  <section class="space-y-4" data-volet-racines>
    <h2 class="font-medium">{{ t('roots_title') }}</h2>

    <p v-if="!lignes.length" class="text-sm text-muted-foreground" data-no-roots>
      {{ t('no_roots') }}
    </p>

    <div
      v-for="(l, i) in lignes"
      :key="l.cle"
      data-root
      class="space-y-2 rounded-md border border-border p-3"
    >
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="l.name"
          data-root-name
          class="w-40"
          :placeholder="t('ph_root_name')"
        />
        <span class="text-xs text-muted-foreground" data-root-kind>
          {{ l.kind === 'local' ? t('kind_local') : t('kind_smb') }}
        </span>
        <span class="text-xs text-muted-foreground" data-root-target>{{ cibleRacine(l) }}</span>
        <!-- L'état du montage est **observé**, jamais saisi : il vient du
             plugin, qui regarde le système de fichiers. -->
        <span v-if="l.kind === 'smb'" class="text-xs" data-root-mounted>
          {{ l.mounted ? t('mounted_yes') : t('mounted_no') }}
        </span>
        <Button
          variant="ghost"
          size="sm"
          class="ml-auto"
          data-root-remove
          :aria-label="t('btn_remove_root')"
          @click="retirer(i)"
        >
          ✕
        </Button>
      </div>

      <div v-if="l.kind === 'local'" class="flex flex-wrap gap-2">
        <Input
          v-model="l.path"
          data-root-path
          class="min-w-64 flex-1"
          :placeholder="t('ph_local_path')"
        />
      </div>

      <div v-else class="flex flex-wrap gap-2">
        <Input
          v-model="l.host"
          data-root-host
          class="w-44"
          :placeholder="t('ph_host')"
        />
        <Input
          v-model="l.share"
          data-root-share
          class="w-40"
          :placeholder="t('ph_share')"
        />
        <Input
          v-model="l.subpath"
          data-root-subpath
          class="w-40"
          :placeholder="t('ph_subpath')"
        />
        <Input
          v-model="l.user"
          data-root-user
          class="w-32"
          :placeholder="t('ph_user')"
        />
        <Input
          v-model="l.password"
          type="password"
          data-root-password
          class="w-32"
          :placeholder="t('ph_password')"
        />
        <Input
          v-model="l.domain"
          data-root-domain
          class="w-28"
          :placeholder="t('ph_domain')"
        />
        <label class="flex items-center gap-1 text-sm">
          <input v-model="l.writable" type="checkbox" data-root-writable />
          {{ t('writable_label') }}
        </label>
      </div>
    </div>

    <!-- Sans cette phrase, un champ mot de passe vide se lit comme un mot de
         passe effacé : l'utilisateur croit devoir le retaper à chaque
         enregistrement, ou pire, croit l'avoir perdu. -->
    <p v-if="aUnPartage" class="text-sm text-muted-foreground" data-password-hint>
      {{ t('password_kept_hint') }}
    </p>

    <div class="flex flex-wrap items-center gap-2">
      <Button variant="secondary" data-add-local :disabled="fige" @click="ajouter('local')">
        {{ t('btn_add_local') }}
      </Button>
      <Button variant="secondary" data-add-share :disabled="fige" @click="ajouter('smb')">
        {{ t('btn_add_share') }}
      </Button>
      <Button data-save-roots :disabled="fige" @click="enregistrer">
        {{ t('btn_save_roots') }}
      </Button>
      <!-- Réconciliation des montages : le plugin ne monte rien de lui-même,
           il demande au service privilégié de le faire. -->
      <Button variant="outline" data-mount :disabled="fige" @click="monter">
        {{ t('btn_mount_now') }}
      </Button>
    </div>
  </section>
</template>
