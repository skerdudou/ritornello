#!/usr/bin/env node
// Anti-regression guardrail wired at the end of `npm run build -w app`. A
// green `vite build` guarantees neither the import map nor the uniqueness
// of the Vue instance: fix round 1 of Task 4 let a
// `ReferenceError: process is not defined` through at `vue.js` load time
// with a build, 36 tests and a `tsc` all green. This script checks the
// three invariants Task 6 depends on.
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'

const distDir = fileURLToPath(new URL('../dist', import.meta.url))
const assetsDir = fileURLToPath(new URL('../dist/assets', import.meta.url))

function echouer(message) {
  console.error(`verifier-dist: ${message}`)
  process.exit(1)
}

// 1. The import map must be present and point at the contract's two
// stable URLs (`vue.js`, `ui-kit.js`): without it, every `import` of
// 'vue' or '@ritornello/ui' resolves to a 404 in the browser, silently
// as far as the build is concerned.
const indexHtml = readFileSync(`${distDir}/index.html`, 'utf8')
if (!indexHtml.includes('<script type="importmap">')) {
  echouer(
    'aucune balise <script type="importmap"> dans dist/index.html — ' +
      'le plugin shellPlugin (vite.config.ts) n\'a peut-etre pas tourne, ' +
      'ou le marqueur <!--IMPORTMAP--> a ete retire de index.html',
  )
}
if (!indexHtml.includes('"vue":"/assets/vue.js"')) {
  echouer(
    'import map presente mais sans entree vers /assets/vue.js — ' +
      'verifier la constante IMPORT_MAP dans vite.config.ts',
  )
}
if (!indexHtml.includes('"@ritornello/ui":"/assets/ui-kit.js"')) {
  echouer(
    'import map presente mais sans entree vers /assets/ui-kit.js — ' +
      'verifier la constante IMPORT_MAP dans vite.config.ts',
  )
}

const fichiers = readdirSync(assetsDir).filter((f) => f.endsWith('.js'))

for (const fichier of fichiers) {
  const contenu = readFileSync(`${assetsDir}/${fichier}`, 'utf8')

  // 2. `build.lib` mode (used for `vue.js` and, kit side, for
  // `ui-kit.js`) does not substitute `process.env.NODE_ENV`: a surviving
  // reference crashes the browser, which has no global `process`. This is
  // exactly fix round 1's Critical.
  if (contenu.includes('process.env')) {
    echouer(
      `${fichier} contient encore "process.env" — ajouter/verifier le define ` +
        "'process.env.NODE_ENV': JSON.stringify('production') dans la config vite.lib " +
        "correspondante (vite.vue.config.ts pour vue.js, web/kit/vite.config.ts pour " +
        'ui-kit.js), puis reconstruire (kit avant app).',
    )
  }

  // 3. The shell's chunks (`app-*.js`) must stay free of any Vue runtime
  // fingerprint: its presence would mean 'vue' was not externalized
  // (rollupOptions.external) and that two Vue instances, hence two
  // reactivity graphs, coexist in the page.
  if (fichier.startsWith('app-')) {
    for (const empreinte of ['__v_isRef', '__v_skip', '[Vue warn]']) {
      if (contenu.includes(empreinte)) {
        echouer(
          `${fichier} contient l'empreinte de runtime Vue "${empreinte}" — ` +
            "verifier que 'vue' figure dans rollupOptions.external de vite.config.ts " +
            'et que ce fichier ne bundle pas Vue en double.',
        )
      }
    }
  }
}

console.log('verifier-dist: import map et unicite du runtime Vue confirmees')
