#!/usr/bin/env node
// Anti-regression guardrail wired at the end of each `crates/*/ui`
// package's `npm run build` (plugin module). Sibling of
// `web/app/scripts/verifier-dist.mjs`: same spirit (a green `vite build`
// guarantees neither the uniqueness of the Vue instance nor the shape of
// the plugins' delivery contract), adapted to the plugin-side invariants
// rather than the shell's:
//   - `ui/dist/` contains only `ui.js` and `ui.css`, flat (the core serves
//     `/plugins/<name>/<file>` with no subdirectory — see README);
//   - `vue` and `@ritornello/ui` really are external imports of `ui.js`,
//     not bundled (otherwise two Vue instances coexist in the page);
//   - no leftover `process.env` (vite's `build.lib` mode does not
//     substitute it; a surviving reference crashes at load time, there
//     being no global `process` in the browser).
//
// This script assumes it is launched with `process.cwd()` equal to the
// package directory (i.e. from a package npm `build` script, not called
// directly from the repo root).
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

// 1. Delivery contract: only ui.js and ui.css, flat. An extra file (or a
// subdirectory, e.g. assets/) matches no core route
// (`/plugins/<name>/<file>`, no subdirectory) and would fail as a silent
// 404 in use.
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

// 2. `build.lib` mode does not substitute `process.env.NODE_ENV`: a
// surviving reference crashes at load time (no global `process` in the
// browser). Same risk as the one documented in
// web/app/scripts/verifier-dist.mjs for vue.js/ui-kit.js.
for (const [nom, contenu] of [['ui.js', uiJs], ['ui.css', uiCss]]) {
  if (contenu.includes('process.env')) {
    echouer(
      `${nom} contient encore "process.env" — verifier le define ` +
        "'process.env.NODE_ENV': JSON.stringify('production') dans vite.config.ts.",
    )
  }
}

// 3. `vue` and `@ritornello/ui` must appear as external imports (no
// bundling of the kit or the Vue runtime into the plugin module).
for (const specifier of ['vue', '@ritornello/ui']) {
  const motif = new RegExp(`from\\s*["']${specifier.replace('/', '\\/')}["']`)
  if (!motif.test(uiJs)) {
    echouer(
      `ui.js ne contient aucun "import ... from '${specifier}'" — ` +
        `verifier que '${specifier}' figure dans rollupOptions.external de vite.config.ts.`,
    )
  }
}

// 4. Same check as (3) but from the consequence's point of view: if the
// Vue runtime had been bundled despite the declared externalization,
// these fingerprints generally survive minification (they are never
// present in plugin code, only in the Vue runtime itself).
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
