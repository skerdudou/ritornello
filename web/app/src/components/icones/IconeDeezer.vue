<!--
  Logo Deezer : ses barres d'égaliseur, réduites à trois hauteurs croissantes.
  Rempli plutôt qu'au trait — des barres de 3 px de large au contour seraient
  illisibles à cette taille.
  Taille intrinseque 20 px, `viewBox` inchange en 15 : la classe du point
  d'appel (`size-5`) pilote le rendu — une regle CSS l'emporte sur un attribut
  de presentation — et l'attribut n'est qu'un repli avant la feuille de style.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 15 15" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <rect x="1" y="9.4" width="3.2" height="2.6" rx="0.5" fill="currentColor" />
    <rect x="5.9" y="6.6" width="3.2" height="5.4" rx="0.5" fill="currentColor" />
    <rect x="10.8" y="3.8" width="3.2" height="8.2" rx="0.5" fill="currentColor" />
    <rect x="10.8" y="1.4" width="3.2" height="1.6" rx="0.5" fill="currentColor" />
  </svg>
</template>
