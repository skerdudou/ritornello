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
 * Popin d'apprentissage d'une touche.
 *
 * Composant purement présentationnel : il ne parle pas au serveur, ne connaît
 * pas la mécanique de sondage ni le délai de 30 s. Il affiche ce qu'on lui
 * donne et émet ce qu'on lui demande — la page qui l'embarque garde seule la
 * vérité (l'état `add`, le sondage, l'annulation côté plugin).
 */
const props = defineProps<{
  ouvert: boolean
  /**
   * Traducteur, tel que `createT` le rend : la clé, et les jetons `{nom}` à y
   * substituer. La signature complète, et non un `(cle) => string` réduit :
   * l'interpolation appartient au traducteur (qui remplace **toutes** les
   * occurrences d'un jeton), pas à ses appelants.
   */
  t: (cle: string, params?: Record<string, string | number>) => string
  /** Libellé **déjà traduit** de l'action apprise, pour le title. */
  action: string
  /** Nom du périphérique, injecté dans la description. */
  device: string
  /** État de la case « add » (v-model:add côté parent). */
  add: boolean
  /**
   * Secondes restantes avant l'abandon, telles que la page les calcule.
   *
   * La popin ne les décompte pas elle-même : l'échéance appartient à la page,
   * qui tient déjà le sondage et l'annulation côté plugin. Un second minuteur
   * ici dériverait du premier, et afficherait un chiffre que rien ne garantit.
   */
  secondes: number
}>()
const emit = defineEmits<{ annuler: []; 'update:add': [boolean] }>()

/**
 * Titre de la popin : le tiret ne sépare que s'il y a une action à nommer.
 *
 * La page remet sa ligne apprise à `null` — donc `action` à la chaîne vide —
 * dès le geste de fermeture, alors que le `Presence` de reka-ui garde le
 * contenu monté le temps du fondu de sortie (`duration-200`). Sans cette
 * garde, le title afficherait « Apprentissage d'une touche — » pendant toute
 * la fermeture.
 */
const title = computed(() =>
  props.action ? `${props.t('dlg_learn_title')} — ${props.action}` : props.t('dlg_learn_title'),
)
</script>

<template>
  <!-- Échap, le clic sur le voile et la croix que `DialogContent` pose par
       défaut ferment le Dialog (`update:open` à `false`) : on les fait passer
       par `annuler`, exactement comme le bouton, pour qu'il n'existe qu'un
       seul chemin d'annulation. -->
  <Dialog :open="props.ouvert" @update:open="(v: boolean) => !v && emit('annuler')">
    <DialogContent data-dlg-learn>
      <DialogHeader>
        <DialogTitle>{{ title }}</DialogTitle>
        <!-- Pas décorative : reka-ui la rattache par `aria-describedby`, et son
             absence laisse un player d'écran annoncer une boîte de dialogue
             dont il ne sait rien dire. -->
        <DialogDescription>
          {{ props.t('dlg_learn_desc', { device: props.device }) }}
        </DialogDescription>
      </DialogHeader>

      <!-- À zéro, plus rien : la page a déjà arrêté l'apprentissage, et un
           « il reste 0 s » affiché pendant le fondu de fermeture serait un
           décompte qui ment. -->
      <p
        v-if="props.secondes > 0"
        data-learn-countdown
        class="text-sm text-muted-foreground"
        aria-live="polite"
      >
        {{ props.t('learn_countdown', { s: props.secondes }) }}
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
        <Button variant="secondary" data-learn-cancel @click="emit('annuler')">
          {{ props.t('btn_cancel') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
