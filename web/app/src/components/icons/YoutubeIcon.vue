<!--
  Official YouTube logo (Simple Icons path, CC0 licence) — no longer a
  hand-made simplification: the owner's decision, a brand keeps ITS colour in
  every palette, light as well as dark. This is the only deliberate exception
  to the UI's "no hard-coded colour" rule (see docs/interface.md, § Player
  card): `fill="#FF0000"` rather than `currentColor`, so `class="size-5"` now
  only drives the size, not the hue.
  The "play" triangle is cut out within the path itself (two sub-paths of
  opposite direction, default `fill-rule`): nothing to carve out here, it is
  the brand's official rendering.
  Intrinsic size 20 px, `viewBox="0 0 24 24"` — the same logical frame as its
  two neighbours (Deezer, Apple Music) for a rendering at the same visual
  scale; the call site's class (`size-5`) drives the real size, the attribute
  is only a fallback before the stylesheet.
-->
<template>
  <svg width="20" height="20" viewBox="0 0 24 24" fill="#FF0000" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <path d="M23.498 6.186a3.016 3.016 0 0 0-2.122-2.136C19.505 3.545 12 3.545 12 3.545s-7.505 0-9.377.505A3.017 3.017 0 0 0 .502 6.186C0 8.07 0 12 0 12s0 3.93.502 5.814a3.016 3.016 0 0 0 2.122 2.136c1.871.505 9.376.505 9.376.505s7.505 0 9.377-.505a3.015 3.015 0 0 0 2.122-2.136C24 15.93 24 12 24 12s0-3.93-.502-5.814zM9.545 15.568V8.432L15.818 12l-6.273 3.568z" />
  </svg>
</template>
