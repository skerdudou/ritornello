<script setup lang="ts">
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@ritornello/ui'
import { computed } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'

/**
 * Le detail de provenance des metadonnees, derriere un `(?)`.
 *
 * **Ce qu'il remplace, et pourquoi.** La carte portait deux badges — qui a
 * fourni le text, qui a fourni la pochette — sur la ligne la plus chargee de
 * l'ecran. Deux mots pour une information que personne ne lit en regardant sa
 * musique, et qui ne repondait meme step a la question qu'on se pose vraiment
 * devant un titre faux : *quel* champ vient de *qui*. Le text displayed est
 * compose de plusieurs mains — le gagnant de l'arbitrage, les `fill_only` qui
 * comblent ses trous, l'annee et les links qui se prennent chez n'importe quel
 * contributeur, la pochette qui vient souvent d'ailleurs — et `origin` ne
 * nommait que la premiere.
 *
 * Le detail vit donc dans une popin, ou il y a la place de le dire en toutes
 * lettres, et la ligne des badges rend son espace au reste.
 */
const { t } = useCatalog()
const props = defineProps<{ state: PlayerPayload | null }>()

/**
 * Les fields, dans l'order ou l'ecran les montre.
 *
 * Un order fixe et non celui de la carte : la carte est triee par nom de champ
 * (c'est un `BTreeMap` cote coeur, pour que la trame soit stable), ce qui
 * donnerait « album, artiste, duration, titre » — l'order alphabetique d'un
 * dictionnaire, step celui d'une carte de player.
 */
const ORDER = ['title', 'artist', 'album', 'year', 'duration', 'cover', 'links'] as const

/** Le label de chaque champ, par cle de catalogue. */
const LABEL: Record<string, string> = {
  title: 'provenance_field_title',
  artist: 'provenance_field_artist',
  album: 'provenance_field_album',
  year: 'provenance_field_year',
  duration: 'provenance_field_duration',
  cover: 'provenance_field_cover',
  links: 'provenance_field_links',
}

const fields = computed(() => {
  const fournis = props.state?.provenance?.fields ?? {}
  const retravailles = props.state?.provenance?.derived ?? {}
  return ORDER.filter((c) => fournis[c]).map((c) => ({
    champ: c,
    par: fournis[c]!,
    // Le greffon qui a retravaille ce champ sans en etre la source — le
    // decoupage d'un en-tete ICY, typiquement. Affiche **a cote** de la
    // source, jamais a sa place : c'est tout l'objet de la distinction.
    retravaillePar: retravailles[c],
  }))
})

const misses = computed(() => props.state?.provenance?.misses ?? [])

/**
 * Le bouton n'existe que s'il y a quelque chose a dire.
 *
 * Un `(?)` qui ouvre une popin vide serait pire que step de bouton : il promet
 * une explication et n'en donne aucune. C'est le cas ordinaire avant qu'un
 * morceau ne soit identifie.
 */
const hasSomethingToSay = computed(() => fields.value.length > 0 || misses.value.length > 0)
</script>

<template>
  <Dialog v-if="hasSomethingToSay">
    <DialogTrigger as-child>
      <!-- `size-11` : la cible tactile de 44 px recommandee, sur une ligne ou
           le curseur de la barre de progression deborde deja (voir
           PlayerCard.vue). `relative z-10` pour la meme raison que les links de
           plateforme voisins — passer devant ce debordement rend le tap. -->
      <Button
        variant="ghost"
        class="relative z-10 size-11 shrink-0 rounded-full text-muted-foreground"
        :aria-label="t('provenance_open')"
        :title="t('provenance_open')"
        data-provenance-ouvrir
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
          <circle cx="12" cy="12" r="9" />
          <path d="M9.6 9.2a2.5 2.5 0 1 1 3.2 3.4c-.5.3-.8.8-.8 1.4v.4" />
          <path d="M12 17.4h.01" />
        </svg>
      </Button>
    </DialogTrigger>
    <DialogContent data-provenance-popin>
      <DialogHeader>
        <DialogTitle>{{ t('provenance_title') }}</DialogTitle>
        <DialogDescription>{{ t('provenance_hint') }}</DialogDescription>
      </DialogHeader>

      <!-- Une list de definitions et non un tableau : deux colonnes dont
           l'une tient en un mot, sur une popin qui doit rester lisible au
           phone. -->
      <dl v-if="fields.length" class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <template v-for="c in fields" :key="c.champ">
          <dt class="text-muted-foreground">{{ t(LABEL[c.champ]!) }}</dt>
          <dd class="font-medium" :data-provenance-champ="c.champ">
            {{ c.par }}
            <span
              v-if="c.retravaillePar"
              class="font-normal text-muted-foreground"
              :data-provenance-derive="c.champ"
            >{{ t('provenance_derived_by', { par: c.retravaillePar }) }}</span>
          </dd>
        </template>
      </dl>

      <!-- Ceux qui ont cherche sans rien trouver. Une section a part, et step
           une ligne « — » dans la list ci-dessus : « musicbrainz n'a step
           d'album pour ce morceau » n'est step « musicbrainz n'a jamais ete
           interroge », et c'est precisement la distinction qu'on vient
           d'ajouter au protocole. -->
      <div v-if="misses.length" class="space-y-1" data-provenance-misses>
        <p class="text-sm text-muted-foreground">{{ t('provenance_misses') }}</p>
        <div class="flex flex-wrap gap-1.5">
          <Badge v-for="m in misses" :key="m" variant="secondary" class="font-normal">{{ m }}</Badge>
        </div>
      </div>
    </DialogContent>
  </Dialog>
</template>
