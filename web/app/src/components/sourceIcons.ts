/**
 * The icon names a source may announce (the core serves them verbatim in
 * `GET /api/presets`). Anything else is drawn as the source's initial — see
 * `SourceIcon.vue`, which holds one drawing per name.
 */
export const SOURCE_ICONS = ['radio', 'disc', 'folder', 'music', 'headphones', 'podcast', 'tv', 'usb'] as const

export type SourceIconName = (typeof SOURCE_ICONS)[number]

export function isSourceIcon(icon: string | null | undefined): icon is SourceIconName {
  return !!icon && (SOURCE_ICONS as readonly string[]).includes(icon)
}
