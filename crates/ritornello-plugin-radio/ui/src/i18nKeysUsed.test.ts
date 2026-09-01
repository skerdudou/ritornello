// Guard rail: the migration to the SPA removed the `PAGE_KEYS` constants
// that used to guarantee that every key used by the UI exists in the
// embedded English catalog. Without that guarantee, a missing key shows up
// verbatim on screen instead of failing at build or test time. This test
// re-collects the keys actually called by this plugin's source code and
// checks their presence in
// `crates/ritornello-plugin-radio/src/locales/en.toml` + the common
// vocabulary `crates/ritornello-i18n/src/locales/common_en.toml` (the
// plugin consumes both, the common layer being merged by
// `Catalog::entries`).
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

// Paths resolved via `process.cwd()` (the package directory, see the npm
// `test` script) rather than via `import.meta.url`: under vitest in a
// `jsdom` environment, a relative URL that climbs above the vite project
// root gets rewritten to `http://localhost/@fs/...` instead of staying a
// `file://` one — `fileURLToPath` then throws `The URL must be of scheme
// file`. `process.cwd()` is a raw OS path, immune to that rewrite.
const PACKAGE_ROOT = process.cwd()

// Hand-written flat TOML reader: the format of the embedded catalogs is a
// simple sequence of `key = "value"` lines, never a `[[...]]` table.
// Sufficient here, no need to add a TOML dependency.
function tomlKeys(path: string): Set<string> {
  const keys = new Set<string>()
  for (const rawLine of readFileSync(path, 'utf8').split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line || line.startsWith('#')) continue
    const i = line.indexOf('=')
    if (i === -1) continue
    const key = line.slice(0, i).trim()
    if (/^[A-Za-z0-9_]+$/.test(key)) keys.add(key)
  }
  return keys
}

function sourceFiles(dir: string): string[] {
  const results: string[] = []
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry)
    if (statSync(path).isDirectory()) {
      results.push(...sourceFiles(path))
    } else if (/\.(vue|ts)$/.test(entry) && !entry.endsWith('.test.ts')) {
      results.push(path)
    }
  }
  return results
}

// Keys called via `t('...')` (template) or `t.value('...')` (script) with a
// literal key. KNOWN, ACCEPTED LIMITATION: a key built dynamically
// (variable, template literal, computed property) would escape this regex.
// This plugin has none so far (no `ACTIONS` list or equivalent, unlike
// generic-input): every key in `RadioAdmin.vue` is a direct literal.
function literalCallKeys(files: string[]): Set<string> {
  const keys = new Set<string>()
  const pattern = /\bt(?:\.value)?\(\s*['"]([A-Za-z0-9_]+)['"]/g
  for (const file of files) {
    const content = readFileSync(file, 'utf8')
    for (const m of content.matchAll(pattern)) if (m[1]) keys.add(m[1])
  }
  return keys
}

describe('i18n keys used by the radio plugin', () => {
  it('all exist in the embedded English catalog (plugin + common)', () => {
    const catalog = new Set([
      ...tomlKeys(resolve(PACKAGE_ROOT, '../src/locales/en.toml')),
      ...tomlKeys(resolve(PACKAGE_ROOT, '../../ritornello-i18n/src/locales/common_en.toml')),
    ])

    const used = literalCallKeys(sourceFiles(join(PACKAGE_ROOT, 'src')))

    const missing = [...used].filter((key) => !catalog.has(key)).sort()
    expect(missing).toEqual([])
  })
})
