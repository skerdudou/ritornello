<!--
  Logo YouTube, simplifié au trait pour tenir dans le même moule que les
  autres icônes : cadre arrondi + triangle de lecture. `aria-hidden`, comme
  ses voisines — c'est le lien qui porte le nom accessible.
  Taille intrinseque 20 px, `viewBox` inchange en 15 : la classe du point
  d'appel (`size-5`) pilote le rendu — une regle CSS l'emporte sur un attribut
  de presentation — et l'attribut n'est qu'un repli avant la feuille de style.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 15 15" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <rect x="0.85" y="2.85" width="13.3" height="9.3" rx="2.6" stroke="currentColor" stroke-width="1.2" />
    <path d="M6.3 5.75 9.75 7.5 6.3 9.25V5.75Z" fill="currentColor" />
  </svg>
</template>
