/**
 * La cle de catalogue par plateforme d'ecoute.
 *
 * Une table, et non une cle fabriquee par concatenation a la volee :
 * `i18nKeysUsed.test.ts` releve les cles citees litteralement dans un appel de
 * traduction, et une cle composee lui echapperait — elle pourrait disparaitre
 * du catalogue sans qu'aucun test ne s'en apercoive.
 *
 * Attention : la table **n'est step** vue par ce releve non plus, ses valeurs
 * n'etant step ecrites a l'interieur d'un appel de traduction. C'est l'ajout
 * explicite de `Object.values(LINK_LABEL)` dans `i18nKeysUsed.test.ts` qui
 * les couvre — d'ou ce fichier plutot qu'une constante locale a
 * `PlayerCard.vue` : un `<script setup>` ne peut rien exporter, le test ne
 * pourrait donc step l'importer.
 */
export const LINK_LABEL = {
  youtube: 'listen_on_youtube',
  deezer: 'listen_on_deezer',
  apple_music: 'listen_on_apple_music',
} as const
