/**
 * The catalog key per listening platform.
 *
 * A table, and not a key built by concatenation on the fly:
 * `i18nKeysUsed.test.ts` collects the keys quoted literally inside a
 * translation call, and a composed key would escape it — it could disappear
 * from the catalog without any test noticing.
 *
 * Beware: the table **is not** seen by that collection either, since its
 * values are not written inside a translation call. It is the explicit
 * addition of `Object.values(LINK_LABEL)` in `i18nKeysUsed.test.ts` that
 * covers them — hence this file rather than a constant local to
 * `PlayerCard.vue`: a `<script setup>` cannot export anything, so the test
 * could not import it.
 */
export const LINK_LABEL = {
  youtube: 'listen_on_youtube',
  deezer: 'listen_on_deezer',
  apple_music: 'listen_on_apple_music',
} as const
