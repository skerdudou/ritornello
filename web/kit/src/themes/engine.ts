import raw from './presets.json'

export type Mode = 'light' | 'dark'

export interface Preset {
  label: string
  styles: { light: Record<string, string>; dark: Record<string, string> }
}

export const presets = raw as unknown as Record<string, Preset>

export const DEFAULT_PRESET = 'northern-lights'
export const DEFAULT_MODE: Mode = 'light'

/** Generic families: cited by the presets but never to be downloaded. */
const GENERICS = new Set([
  'sans-serif', 'serif', 'monospace', 'system-ui', 'ui-monospace', 'ui-serif',
  'ui-sans-serif', 'cursive', 'fantasy', 'inherit',
])

/**
 * Fallback appended at the end of the stack per typographic family, so that
 * the UI stays readable when the font CDN is unreachable (device offline):
 * it is the interface's only external resource.
 */
const FALLBACKS: Record<string, string> = {
  'font-sans': 'system-ui, sans-serif',
  'font-serif': 'ui-serif, serif',
  'font-mono': 'ui-monospace, monospace',
}

/**
 * The `light` block serves as the base, the mode's block overrides it: the
 * upstream `dark` blocks most often omit the non-chromatic keys (fonts,
 * radius), which must then come from the light block.
 */
export function resolveVars(preset: Preset, mode: Mode): Record<string, string> {
  return { ...preset.styles.light, ...preset.styles[mode] }
}

export function withFallback(key: string, value: string): string {
  const fallback = FALLBACKS[key]
  if (!fallback) return value
  const already = value
    .split(',')
    .some((part) => GENERICS.has(part.trim().toLowerCase()))
  return already ? value : `${value}, ${fallback}`
}

export function fontFamilies(vars: Record<string, string>): string[] {
  const out: string[] = []
  for (const key of Object.keys(FALLBACKS)) {
    const value = vars[key]
    if (!value) continue
    const first = value.split(',')[0]?.trim().replace(/^["']|["']$/g, '')
    if (!first || GENERICS.has(first.toLowerCase())) continue
    if (!out.includes(first)) out.push(first)
  }
  return out
}

/**
 * A single font link lives in the document: it is replaced at every theme
 * application (marked by `data-ritornello-fonts`). No font is embedded in the
 * binaries — see the spec.
 */
function ensureFontLink(families: string[], doc: Document): void {
  const existing = doc.head.querySelector('link[data-ritornello-fonts]')
  if (existing) existing.remove()
  if (families.length === 0) return
  const families_url = families
    .map((f) => `family=${encodeURIComponent(f).replace(/%20/g, '+')}:wght@400;500;600;700`)
    .join('&')
  const link = doc.createElement('link')
  link.rel = 'stylesheet'
  link.setAttribute('data-ritornello-fonts', '')
  link.href = `https://fonts.googleapis.com/css2?${families_url}&display=swap`
  doc.head.appendChild(link)
}

/**
 * Keys set by the last application, per `root`, so they can be removed: a
 * preset that does not define a variable must not inherit it from the previous
 * preset. Indexed by `root` (and not a single global state) because several
 * roots may receive different themes: purging from a shared list would leak
 * the keys of one root into another. A `WeakMap` is used so as not to keep a
 * destroyed root in memory.
 */
const applied = new WeakMap<HTMLElement, string[]>()

/**
 * Writes each entry of the resolved preset as a CSS variable on `root`,
 * **generic** iteration: no hard-coded list of keys, so that an upstream preset
 * gaining a variable works without touching the code.
 *
 * `root`, `doc` and `catalog` are only parameterizable for the tests.
 */
export function applyTheme(
  id: string,
  mode: Mode,
  root: HTMLElement = document.documentElement,
  doc: Document = document,
  catalog: Record<string, Preset> = presets,
): void {
  const preset = catalog[id]
  if (!preset) {
    console.warn(`unknown theme ignored: ${id}`)
    return
  }
  for (const key of applied.get(root) ?? []) root.style.removeProperty(`--${key}`)
  const vars = resolveVars(preset, mode)
  for (const [key, value] of Object.entries(vars)) {
    root.style.setProperty(`--${key}`, withFallback(key, value))
  }
  applied.set(root, Object.keys(vars))
  root.classList.toggle('dark', mode === 'dark')
  ensureFontLink(fontFamilies(vars), doc)
}
