<!--
  Logo Deezer : ses barres d'egaliseur, tracé maison — Simple Icons ne
  distribue plus ce logo (retiré à la demande de la marque), donc pas de
  tracé officiel disponible ici. Meme exception assumee qu'IconeYoutube.vue
  et IconeAppleMusic.vue : `fill="#A238FF"` en dur (violet Deezer) plutot
  que `currentColor` — voir docs/interface.md, § Player card.
  `viewBox` porte de 15x15 a 24x24 (memes proportions, x1.6) pour partager
  le meme repere logique que les deux traces Simple Icons voisins.
  Rempli plutot qu'au trait — des barres de 3 px de large au contour
  seraient illisibles a cette taille.
  Taille intrinseque 20 px : la classe du point d'appel (`size-5`) pilote
  le rendu reel, l'attribut n'est qu'un repli avant la feuille de style.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <rect x="1.6" y="15.04" width="5.12" height="4.16" rx="0.8" fill="#A238FF" />
    <rect x="9.44" y="10.56" width="5.12" height="8.64" rx="0.8" fill="#A238FF" />
    <rect x="17.28" y="6.08" width="5.12" height="13.12" rx="0.8" fill="#A238FF" />
    <rect x="17.28" y="2.24" width="5.12" height="2.56" rx="0.8" fill="#A238FF" />
  </svg>
</template>
