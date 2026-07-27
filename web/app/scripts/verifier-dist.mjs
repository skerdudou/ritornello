#!/usr/bin/env node
// Garde-fou anti-regression branche en fin de `npm run build -w app`. Un
// `vite build` vert ne garantit ni l'import map ni l'unicite de l'instance
// de Vue : le round de correction 1 de la Task 4 a laisse passer un
// `ReferenceError: process is not defined` au chargement de `vue.js` avec
// un build, 36 tests et un `tsc` tous verts. Ce script verifie les trois
// invariants dont depend la Task 6.
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'

const distDir = fileURLToPath(new URL('../dist', import.meta.url))
const assetsDir = fileURLToPath(new URL('../dist/assets', import.meta.url))

function echouer(message) {
  console.error(`verifier-dist: ${message}`)
  process.exit(1)
}

// 1. L'import map doit etre presente et pointer vers les deux URL stables du
// contrat (`vue.js`, `ui-kit.js`) : sans elle, tout `import` de 'vue' ou de
// '@ritornello/ui' resout en 404 dans le navigateur, en silence pour le build.
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

  // 2. Le mode `build.lib` (utilise pour `vue.js` et, cote kit, pour
  // `ui-kit.js`) ne substitue pas `process.env.NODE_ENV` : une reference qui
  // survit fait planter le navigateur, qui n'a pas de `process` global. C'est
  // exactement le Critical du round de correction 1.
  if (contenu.includes('process.env')) {
    echouer(
      `${fichier} contient encore "process.env" — ajouter/verifier le define ` +
        "'process.env.NODE_ENV': JSON.stringify('production') dans la config vite.lib " +
        "correspondante (vite.vue.config.ts pour vue.js, web/kit/vite.config.ts pour " +
        'ui-kit.js), puis reconstruire (kit avant app).',
    )
  }

  // 3. Les chunks du shell (`app-*.js`) doivent rester vides de toute
  // empreinte du runtime Vue : sa presence signalerait que 'vue' n'a pas ete
  // externalise (rollupOptions.external) et que deux instances de Vue, donc
  // deux graphes de reactivite, coexistent dans la page.
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
