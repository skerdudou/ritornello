// Guard rail borrowed from the radio plugin: every key called by the UI must
// exist in the plugin's embedded English catalog
// (`crates/ritornello-plugin-files/src/locales/en.toml`) or in the common
// vocabulary (`crates/ritornello-i18n/src/locales/common_en.toml`), which
// `Catalog::entries` merges. Without this guarantee, a missing key displays
// **as is** on screen — the user reads "btn_mount_now" — instead of failing
// the build or the tests.
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

// Paths resolved via `process.cwd()` (the package's directory, see the npm
// `test` script) rather than via `import.meta.url`: under vitest in a
// `jsdom` environment, a relative URL that climbs out of the vite project
// root is rewritten as `http://localhost/@fs/...` instead of staying a
// `file://` one, and `fileURLToPath` then throws "The URL must be of scheme
// file".
const PACKAGE_ROOT = process.cwd()

/**
 * Page keys **not yet** in the server's catalogs.
 *
 * They will be added there together with their French translation: the two
 * files (`src/locales/en.toml` and `deploy/locales/files/fr.toml`) are kept
 * at parity by a Rust-side test, so a key only enters both at once. This
 * list is the contract between this module and that effort, and it is
 * checked both ways: a key listed here while it now exists in the catalog
 * must be removed from it, or else the list would turn into a permanent
 * hole in the guard rail.
 */
const PENDING: string[] = []

// Flat TOML reader written by hand: the embedded catalogs' format is a
// plain sequence of `key = "value"` lines, never a `[[...]]` table.
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
// literal key. KNOWN LIMITATION, accepted: a dynamically built key would
// escape this expression. This page has none — all its keys are literals,
// including in ternary branches.
function keysFromLiteralCalls(files: string[]): Set<string> {
  const keys = new Set<string>()
  const pattern = /\bt(?:\.value)?\(\s*['"]([A-Za-z0-9_]+)['"]/g
  for (const file of files) {
    for (const m of readFileSync(file, 'utf8').matchAll(pattern)) if (m[1]) keys.add(m[1])
  }
  return keys
}

describe('i18n keys used by the files plugin', () => {
  const catalog = new Set([
    ...tomlKeys(resolve(PACKAGE_ROOT, '../src/locales/en.toml')),
    ...tomlKeys(resolve(PACKAGE_ROOT, '../../ritornello-i18n/src/locales/common_en.toml')),
  ])

  it('introduces none outside the catalog and outside the pending list', () => {
    const used = keysFromLiteralCalls(sourceFiles(join(PACKAGE_ROOT, 'src')))
    const pending = new Set(PENDING)
    const missing = [...used].filter((c) => !catalog.has(c) && !pending.has(c)).sort()
    expect(missing).toEqual([])
  })

  it('only keeps keys pending that are genuinely absent from the catalog', () => {
    // Without this half, `PENDING` would become a permanent hole: a key
    // removed from the code or added to the catalog would stay in it, and
    // would mask the next real absence bearing the same name.
    const arrived = PENDING.filter((c) => catalog.has(c))
    expect(arrived).toEqual([])
  })
})
