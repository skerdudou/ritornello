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

function fail(message) {
  console.error(`check-dist: ${message}`)
  process.exit(1)
}

// 1. The import map must be present and point at the contract's two
// stable URLs (`vue.js`, `ui-kit.js`): without it, every `import` of
// 'vue' or '@ritornello/ui' resolves to a 404 in the browser, silently
// as far as the build is concerned.
const indexHtml = readFileSync(`${distDir}/index.html`, 'utf8')
if (!indexHtml.includes('<script type="importmap">')) {
  fail(
    'no <script type="importmap"> tag in dist/index.html — ' +
      'the shellPlugin plugin (vite.config.ts) may not have run, ' +
      'or the <!--IMPORTMAP--> marker was removed from index.html',
  )
}
if (!indexHtml.includes('"vue":"/assets/vue.js"')) {
  fail(
    'import map present but without an entry to /assets/vue.js — ' +
      'check the IMPORT_MAP constant in vite.config.ts',
  )
}
if (!indexHtml.includes('"@ritornello/ui":"/assets/ui-kit.js"')) {
  fail(
    'import map present but without an entry to /assets/ui-kit.js — ' +
      'check the IMPORT_MAP constant in vite.config.ts',
  )
}

const files = readdirSync(assetsDir).filter((f) => f.endsWith('.js'))

for (const file of files) {
  const content = readFileSync(`${assetsDir}/${file}`, 'utf8')

  // 2. `build.lib` mode (used for `vue.js` and, kit side, for
  // `ui-kit.js`) does not substitute `process.env.NODE_ENV`: a surviving
  // reference crashes the browser, which has no global `process`. This is
  // exactly fix round 1's Critical.
  if (content.includes('process.env')) {
    fail(
      `${file} still contains "process.env" — add/check the define ` +
        "'process.env.NODE_ENV': JSON.stringify('production') in the matching vite.lib " +
        "config (vite.vue.config.ts for vue.js, web/kit/vite.config.ts for " +
        'ui-kit.js), then rebuild (kit before app).',
    )
  }

  // 3. The shell's chunks (`app-*.js`) must stay free of any Vue runtime
  // fingerprint: its presence would mean 'vue' was not externalized
  // (rollupOptions.external) and that two Vue instances, hence two
  // reactivity graphs, coexist in the page.
  if (file.startsWith('app-')) {
    for (const fingerprint of ['__v_isRef', '__v_skip', '[Vue warn]']) {
      if (content.includes(fingerprint)) {
        fail(
          `${file} contains the Vue runtime fingerprint "${fingerprint}" — ` +
            "check that 'vue' is listed in rollupOptions.external of vite.config.ts " +
            'and that this file does not bundle Vue twice.',
        )
      }
    }
  }
}

console.log('check-dist: import map and Vue runtime uniqueness confirmed')
