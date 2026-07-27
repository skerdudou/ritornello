// Reexport de Vue, bati a part sous un nom stable (`assets/vue.js`) pour
// etre la cible de l'import map. Tous les bundles (shell, kit, modules de
// plugin) marquent `vue` comme externe et le resolvent ici : une seule
// instance de Vue vit dans la page, donc une seule reactivite et un seul
// arbre de `provide`/`inject`.
export * from 'vue'
