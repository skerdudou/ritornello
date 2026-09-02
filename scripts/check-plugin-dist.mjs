#!/usr/bin/env node
// Anti-regression guardrail wired at the end of each `crates/*/ui`
// package's `npm run build` (plugin module). Sibling of
// `web/app/scripts/check-dist.mjs`: same spirit (a green `vite build`
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

function fail(message) {
  console.error(`check-plugin-dist: ${message}`)
  process.exit(1)
}

let entries
try {
  entries = readdirSync(distDir)
} catch {
  fail(`cannot read ${distDir} — was the build run before this script?`)
}

// 1. Delivery contract: only ui.js and ui.css, flat. An extra file (or a
// subdirectory, e.g. assets/) matches no core route
// (`/plugins/<name>/<file>`, no subdirectory) and would fail as a silent
// 404 in use.
const expected = new Set(['ui.js', 'ui.css'])
const unexpected = entries.filter((f) => !expected.has(f))
if (unexpected.length > 0) {
  fail(
    `dist/ has extra entries: ${unexpected.join(', ')} — ` +
      "only 'ui.js' and 'ui.css' are expected, flat; check " +
      'rollupOptions.output.assetFileNames and build.lib.fileName in vite.config.ts',
  )
}
for (const file of expected) {
  if (!entries.includes(file)) {
    fail(`dist/${file} is missing — did the build fail silently?`)
  }
}

const uiJs = readFileSync(`${distDir}/ui.js`, 'utf8')
const uiCss = readFileSync(`${distDir}/ui.css`, 'utf8')

// 2. `build.lib` mode does not substitute `process.env.NODE_ENV`: a
// surviving reference crashes at load time (no global `process` in the
// browser). Same risk as the one documented in
// web/app/scripts/check-dist.mjs for vue.js/ui-kit.js.
for (const [name, content] of [['ui.js', uiJs], ['ui.css', uiCss]]) {
  if (content.includes('process.env')) {
    fail(
      `${name} still contains "process.env" — check the define ` +
        "'process.env.NODE_ENV': JSON.stringify('production') in vite.config.ts",
    )
  }
}

// 3. `vue` and `@ritornello/ui` must appear as external imports (no
// bundling of the kit or the Vue runtime into the plugin module).
for (const specifier of ['vue', '@ritornello/ui']) {
  const pattern = new RegExp(`from\\s*["']${specifier.replace('/', '\\/')}["']`)
  if (!pattern.test(uiJs)) {
    fail(
      `ui.js contains no "import ... from '${specifier}'" — ` +
        `check that '${specifier}' is listed in rollupOptions.external in vite.config.ts`,
    )
  }
}

// 4. Same check as (3) but from the consequence's point of view: if the
// Vue runtime had been bundled despite the declared externalization,
// these fingerprints generally survive minification (they are never
// present in plugin code, only in the Vue runtime itself).
for (const fingerprint of ['__v_isRef', '__v_skip', '[Vue warn]']) {
  if (uiJs.includes(fingerprint)) {
    fail(
      `ui.js contains the Vue runtime fingerprint "${fingerprint}" — ` +
        "check that 'vue' is listed in rollupOptions.external in vite.config.ts " +
        'and that this file does not bundle a second Vue',
    )
  }
}

console.log('check-plugin-dist: delivery contract and externals confirmed')
