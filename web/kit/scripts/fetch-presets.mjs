// Converts tweakcn's `utils/theme-presets.ts` (Apache-2.0,
// https://github.com/jnsahaj/tweakcn) into `src/themes/presets.json`.
//
// Upstream is a TypeScript module whose body is a pure object literal: we
// reduce it to JSON by stripping the import, the `export const`
// declaration and the trailing commas, then validate it by parsing it.
// Deliberately naive: if upstream changes shape, the script fails loudly
// rather than producing partial JSON.
//
// Usage: node scripts/fetch-presets.mjs
import { writeFileSync } from 'node:fs'

const URL_AMONT =
  'https://raw.githubusercontent.com/jnsahaj/tweakcn/main/utils/theme-presets.ts'

const source = await fetch(URL_AMONT).then((r) => {
  if (!r.ok) throw new Error(`amont injoignable : HTTP ${r.status}`)
  return r.text()
})

const debut = source.indexOf('{', source.indexOf('defaultPresets'))
if (debut < 0) throw new Error("`defaultPresets` introuvable dans l'amont")
const corps = source
  .slice(debut)
  .replace(/;?\s*$/, '')
  .replace(/,(\s*[}\]])/g, '$1')          // trailing commas
  .replace(/([{,]\s*)([A-Za-z_$][\w$]*)\s*:/g, '$1"$2":')  // bare keys
  // some font-stack values are single-quoted (with double quotes inside,
  // e.g. '"Lora", Georgia, serif'): re-quote them as JSON double quotes,
  // escaping the inner ones.
  .replace(/'((?:[^'\\]|\\.)*)'/g, (_m, inner) => `"${inner.replace(/"/g, '\\"')}"`)

const presets = JSON.parse(corps)
const noms = Object.keys(presets)
if (noms.length < 40) throw new Error(`trop peu de presets analysés : ${noms.length}`)
if (!presets['northern-lights']) throw new Error('`northern-lights` absent')
for (const [nom, p] of Object.entries(presets)) {
  if (!p.label || !p.styles?.light || !p.styles?.dark) {
    throw new Error(`preset ${nom} incomplet`)
  }
}

writeFileSync(
  new URL('../src/themes/presets.json', import.meta.url),
  JSON.stringify(presets, null, 2) + '\n',
)
console.log(`${noms.length} presets écrits`)
