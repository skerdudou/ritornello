<!--
  Apple Music : la note dans son cadre arrondi. Volontairement la note et non
  la pomme — une pomme rendue au trait dans 20 px devient une tache, là où la
  note reste lisible et reste la moitié reconnaissable de la marque.
  Taille intrinseque 20 px, `viewBox` inchange en 15 : la classe du point
  d'appel (`size-5`) pilote le rendu — une regle CSS l'emporte sur un attribut
  de presentation — et l'attribut n'est qu'un repli avant la feuille de style.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 15 15" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <rect x="0.85" y="0.85" width="13.3" height="13.3" rx="3.4" stroke="currentColor" stroke-width="1.2" />
    <path
      d="M9.9 3.6 6.4 4.4a0.6 0.6 0 0 0-0.5 0.6v4.2a1.5 1.5 0 1 0 0.9 1.4V6.1l2.6-0.6v2.6a1.5 1.5 0 1 0 0.9 1.4V4.2a0.6 0.6 0 0 0-0.4-0.6Z"
      fill="currentColor"
    />
  </svg>
</template>
