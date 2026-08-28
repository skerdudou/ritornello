<!--
  Logo YouTube officiel (tracé Simple Icons, licence CC0) — plus une
  simplification maison : décision du propriétaire, une marque garde SA
  couleur dans toutes les palettes, claire comme sombre. C'est la seule
  exception assumée à la règle « aucune couleur en dur » de l'IHM (voir
  docs/interface.md, § Player card) : `fill="#FF0000"` plutôt que
  `currentColor`, donc `class="size-5"` ne pilote plus que la taille, pas
  la teinte.
  Le triangle « play » est en creux dans le tracé lui-meme (deux
  sous-chemins de sens oppose, `fill-rule` par defaut) : rien a decouper
  ici, c'est le rendu officiel de la marque.
  Taille intrinseque 20 px, `viewBox="0 0 24 24"` — le meme repere logique
  que ses deux voisines (Deezer, Apple Music) pour un rendu a la meme
  echelle visuelle ; la classe du point d'appel (`size-5`) pilote la
  taille reelle, l'attribut n'est qu'un repli avant la feuille de style.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 24 24" fill="#FF0000" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <path d="M23.498 6.186a3.016 3.016 0 0 0-2.122-2.136C19.505 3.545 12 3.545 12 3.545s-7.505 0-9.377.505A3.017 3.017 0 0 0 .502 6.186C0 8.07 0 12 0 12s0 3.93.502 5.814a3.016 3.016 0 0 0 2.122 2.136c1.871.505 9.376.505 9.376.505s7.505 0 9.377-.505a3.015 3.015 0 0 0 2.122-2.136C24 15.93 24 12 24 12s0-3.93-.502-5.814zM9.545 15.568V8.432L15.818 12l-6.273 3.568z" />
  </svg>
</template>
