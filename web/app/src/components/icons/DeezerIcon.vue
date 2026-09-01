<!--
  Deezer logo: its equaliser bars, hand-drawn path — Simple Icons no longer
  distributes this logo (removed at the brand's request), so no official path
  is available here. Same deliberate exception as YoutubeIcon.vue and
  AppleMusicIcon.vue: hard-coded `fill="#A238FF"` (Deezer purple) rather than
  `currentColor` — see docs/interface.md, § Player card.
  `viewBox` scaled from 15x15 to 24x24 (same proportions, x1.6) to share the
  same logical frame as the two neighbouring Simple Icons paths.
  Filled rather than stroked — 3 px wide bars drawn as outlines would be
  unreadable at this size.
  Intrinsic size 20 px: the call site's class (`size-5`) drives the real
  rendering, the attribute is only a fallback before the stylesheet.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <rect x="1.6" y="15.04" width="5.12" height="4.16" rx="0.8" fill="#A238FF" />
    <rect x="9.44" y="10.56" width="5.12" height="8.64" rx="0.8" fill="#A238FF" />
    <rect x="17.28" y="6.08" width="5.12" height="13.12" rx="0.8" fill="#A238FF" />
    <rect x="17.28" y="2.24" width="5.12" height="2.56" rx="0.8" fill="#A238FF" />
  </svg>
</template>
