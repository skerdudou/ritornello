import brut from './presets.json'

export type Mode = 'light' | 'dark'

export interface Preset {
  label: string
  styles: { light: Record<string, string>; dark: Record<string, string> }
}

export const presets = brut as unknown as Record<string, Preset>

export const DEFAULT_PRESET = 'northern-lights'
export const DEFAULT_MODE: Mode = 'light'

/** Familles génériques : citées par les presets mais jamais à télécharger. */
const GENERICS = new Set([
  'sans-serif', 'serif', 'monospace', 'system-ui', 'ui-monospace', 'ui-serif',
  'ui-sans-serif', 'cursive', 'fantasy', 'inherit',
])

/**
 * Repli ajouté en fin de pile par famille typographique, pour que l'IHM
 * reste lisible quand le CDN de polices est injoignable (appareil hors
 * ligne) : c'est la seule ressource externe de l'interface.
 */
const FALLBACKS: Record<string, string> = {
  'font-sans': 'system-ui, sans-serif',
  'font-serif': 'ui-serif, serif',
  'font-mono': 'ui-monospace, monospace',
}

/**
 * Le bloc `light` sert de base, le bloc du mode le surcharge : les blocs
 * `dark` de l'amont omettent le plus souvent les clés non chromatiques
 * (polices, rayon), qui doivent alors venir du bloc clair.
 */
export function resolveVars(preset: Preset, mode: Mode): Record<string, string> {
  return { ...preset.styles.light, ...preset.styles[mode] }
}

export function withFallback(key: string, value: string): string {
  const repli = FALLBACKS[key]
  if (!repli) return value
  const deja = value
    .split(',')
    .some((part) => GENERICS.has(part.trim().toLowerCase()))
  return deja ? value : `${value}, ${repli}`
}

export function fontFamilies(vars: Record<string, string>): string[] {
  const out: string[] = []
  for (const key of Object.keys(FALLBACKS)) {
    const value = vars[key]
    if (!value) continue
    const premiere = value.split(',')[0]?.trim().replace(/^["']|["']$/g, '')
    if (!premiere || GENERICS.has(premiere.toLowerCase())) continue
    if (!out.includes(premiere)) out.push(premiere)
  }
  return out
}

/**
 * Un seul lien de polices vit dans le document : il est remplacé à chaque
 * application de thème (marqué par `data-ritornello-fonts`). Aucune police
 * n'est embarquée dans les binaires — voir la spec.
 */
function ensureFontLink(familles: string[], doc: Document): void {
  const existant = doc.head.querySelector('link[data-ritornello-fonts]')
  if (existant) existant.remove()
  if (familles.length === 0) return
  const familles_url = familles
    .map((f) => `family=${encodeURIComponent(f).replace(/%20/g, '+')}:wght@400;500;600;700`)
    .join('&')
  const link = doc.createElement('link')
  link.rel = 'stylesheet'
  link.setAttribute('data-ritornello-fonts', '')
  link.href = `https://fonts.googleapis.com/css2?${familles_url}&display=swap`
  doc.head.appendChild(link)
}

/**
 * Clés posées par la dernière application, par `root`, pour pouvoir les
 * retirer : un preset qui ne définit pas une variable ne doit pas hériter de
 * celle du preset précédent. Indexé par `root` (et non un état global unique)
 * car plusieurs roots peuvent recevoir des thèmes différents : purger d'après
 * une liste partagée ferait fuiter les clés d'un root vers un autre. Une
 * `WeakMap` est utilisée pour ne pas retenir en mémoire un root détruit.
 */
const applied = new WeakMap<HTMLElement, string[]>()

/**
 * Écrit chaque entrée du preset résolu en variable CSS sur `root`, itération
 * **générique** : aucune liste de clés en dur, pour qu'un preset amont qui
 * gagne une variable fonctionne sans toucher au code.
 *
 * `root`, `doc` et `catalogue` ne sont paramétrables que pour les tests.
 */
export function applyTheme(
  id: string,
  mode: Mode,
  root: HTMLElement = document.documentElement,
  doc: Document = document,
  catalogue: Record<string, Preset> = presets,
): void {
  const preset = catalogue[id]
  if (!preset) {
    console.warn(`thème inconnu ignoré : ${id}`)
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
