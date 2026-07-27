// Convertit `utils/theme-presets.ts` de tweakcn (Apache-2.0,
// https://github.com/jnsahaj/tweakcn) en `src/themes/presets.json`.
//
// L'amont est un module TypeScript dont le corps est un littéral d'objet pur :
// on le réduit en JSON en retirant l'import, la déclaration `export const` et
// les virgules traînantes, puis on le valide en le parsant. Volontairement
// naïf : si l'amont change de forme, le script échoue bruyamment plutôt que
// de produire un JSON partiel.
//
// Usage : node scripts/fetch-presets.mjs
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
  .replace(/,(\s*[}\]])/g, '$1')          // virgules traînantes
  .replace(/([{,]\s*)([A-Za-z_$][\w$]*)\s*:/g, '$1"$2":')  // clés nues
  // certaines valeurs de pile de polices sont en quotes simples (avec des
  // guillemets doubles à l'intérieur, ex. '"Lora", Georgia, serif') : on les
  // repasse en quotes doubles JSON en échappant les guillemets internes.
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
