#!/usr/bin/env node
// Garde-fou anti-regression branche en fin de `npm run build` de chaque
// paquet `crates/*/ui` (module de plugin). Frere de
// `web/app/scripts/verifier-dist.mjs` : meme esprit (un `vite build` vert
// ne garantit ni l'unicite de l'instance de Vue ni la forme du contrat de
// livraison des plugins), adapte aux invariants du cote plugin plutot que
// du shell :
//   - `ui/dist/` ne contient que `ui.js` et `ui.css`, a plat (le coeur sert
//     `/plugins/<nom>/<fichier>` sans sous-repertoire — voir README) ;
//   - `vue` et `@ritornello/ui` sont bien des imports externes de `ui.js`,
//     pas embarques (sinon deux instances de Vue coexistent dans la page) ;
//   - aucun `process.env` residuel (le mode `build.lib` de vite ne le
//     substitue pas ; une reference qui survit plante au chargement, faute
//     de `process` global dans le navigateur).
//
// Ce script suppose etre lance avec `process.cwd()` egal au repertoire du
// paquet (i.e. depuis un script npm `build` du paquet, pas appele
// directement depuis la racine du repo).
import { readFileSync, readdirSync } from 'node:fs'

const distDir = `${process.cwd()}/dist`

function echouer(message) {
  console.error(`verifier-dist-plugin: ${message}`)
  process.exit(1)
}

let entrees
try {
  entrees = readdirSync(distDir)
} catch {
  echouer(`impossible de lire ${distDir} — le build a-t-il ete lance avant ce script ?`)
}

// 1. Contrat de livraison : uniquement ui.js et ui.css, a plat. Un fichier
// supplementaire (ou un sous-repertoire, ex. assets/) ne correspond a aucune
// route du coeur (`/plugins/<nom>/<fichier>`, sans sous-repertoire) et
// echouerait en 404 silencieux a l'usage.
const attendu = new Set(['ui.js', 'ui.css'])
const inattendus = entrees.filter((f) => !attendu.has(f))
if (inattendus.length > 0) {
  echouer(
    `dist/ contient des entrees en trop : ${inattendus.join(', ')} — ` +
      "seuls 'ui.js' et 'ui.css' sont attendus, a plat. Verifier " +
      'rollupOptions.output.assetFileNames et build.lib.fileName dans vite.config.ts.',
  )
}
for (const fichier of attendu) {
  if (!entrees.includes(fichier)) {
    echouer(`dist/${fichier} est absent — le build a-t-il echoue silencieusement ?`)
  }
}

const uiJs = readFileSync(`${distDir}/ui.js`, 'utf8')
const uiCss = readFileSync(`${distDir}/ui.css`, 'utf8')

// 2. Le mode `build.lib` ne substitue pas `process.env.NODE_ENV` : une
// reference qui survit plante au chargement (pas de `process` global dans
// le navigateur). C'est le meme risque que celui documente dans
// web/app/scripts/verifier-dist.mjs pour vue.js/ui-kit.js.
for (const [nom, contenu] of [['ui.js', uiJs], ['ui.css', uiCss]]) {
  if (contenu.includes('process.env')) {
    echouer(
      `${nom} contient encore "process.env" — verifier le define ` +
        "'process.env.NODE_ENV': JSON.stringify('production') dans vite.config.ts.",
    )
  }
}

// 3. `vue` et `@ritornello/ui` doivent apparaitre comme imports externes
// (pas de bundling du kit ni du runtime Vue dans le module de plugin).
for (const specifier of ['vue', '@ritornello/ui']) {
  const motif = new RegExp(`from\\s*["']${specifier.replace('/', '\\/')}["']`)
  if (!motif.test(uiJs)) {
    echouer(
      `ui.js ne contient aucun "import ... from '${specifier}'" — ` +
        `verifier que '${specifier}' figure dans rollupOptions.external de vite.config.ts.`,
    )
  }
}

// 4. Meme verification que (3) mais du point de vue de la consequence :
// si le runtime Vue avait ete embarque malgre l'externalisation declaree,
// ces empreintes surviennent generalement a la minification (elles ne sont
// jamais presentes dans du code de plugin, seulement dans le runtime Vue
// lui-meme).
for (const empreinte of ['__v_isRef', '__v_skip', '[Vue warn]']) {
  if (uiJs.includes(empreinte)) {
    echouer(
      `ui.js contient l'empreinte de runtime Vue "${empreinte}" — ` +
        "verifier que 'vue' figure dans rollupOptions.external de vite.config.ts " +
        'et que ce fichier ne bundle pas Vue en double.',
    )
  }
}

console.log('verifier-dist-plugin: contrat de livraison et externalites confirmes')
