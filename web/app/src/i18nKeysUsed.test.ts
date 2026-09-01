// Guardrail: the migration to the SPA removed the `PAGE_KEYS` constants that
// guaranteed every key used by the UI exists in the embedded English
// catalog. Without that guarantee, a missing key is displayed as-is on
// screen instead of failing at build or test time (this is exactly what
// happened for three shell messages, see the comment in PluginView.ts).
// This test re-collects the keys actually called by the source code and
// checks their presence in `crates/ritornello-core/src/locales/en.toml` +
// the common vocabulary `crates/ritornello-i18n/src/locales/common_en.toml`
// (the shell consumes both, the common layer being merged by
// `Catalog::entries`).
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import { LINK_LABEL } from './components/links'
import { REMOTE_COMMANDS } from './views/remoteCommands'

// Paths resolved via `process.cwd()` (the package directory, cf. the npm
// `test` script) rather than via `import.meta.url`: under vitest in a `jsdom`
// environment, a relative URL that climbs out of the vite project root is
// rewritten to `http://localhost/@fs/...` instead of staying a `file://` —
// `fileURLToPath` then throws `The URL must be of scheme file`.
// `process.cwd()` is a raw OS path, immune to that rewriting.
const PACKAGE_ROOT = process.cwd()

// Hand-written flat TOML reader: the format of the embedded catalogs is a
// plain sequence of `key = "value"` lines, never a `[[...]]` table. Enough
// here, no need to add a TOML dependency.
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
// literal key. KNOWN LIMIT, accepted: a dynamically built key (variable,
// template literal, computed property, such as `t(c.key)` or
// `t(error.value)`) escapes this regex. That is why the keys below that are
// never hard-coded in a `t(...)` call are added explicitly further down.
function literalCallKeys(files: string[]): Set<string> {
  const keys = new Set<string>()
  const pattern = /\bt(?:\.value)?\(\s*['"]([A-Za-z0-9_]+)['"]/g
  for (const file of files) {
    const content = readFileSync(file, 'utf8')
    for (const m of content.matchAll(pattern)) if (m[1]) keys.add(m[1])
  }
  return keys
}

describe('i18n keys used by the shell', () => {
  it('all exist in the embedded English catalog (core + common)', () => {
    const catalog = new Set([
      ...tomlKeys(resolve(PACKAGE_ROOT, '../../crates/ritornello-core/src/locales/en.toml')),
      ...tomlKeys(
        resolve(PACKAGE_ROOT, '../../crates/ritornello-i18n/src/locales/common_en.toml'),
      ),
    ])

    const used = new Set([
      ...literalCallKeys(sourceFiles(join(PACKAGE_ROOT, 'src'))),
      // views/remoteCommands.ts: indexed by `.key`, never hard-coded in a
      // `t(...)` call (HomeView.vue does `t(c.key)`).
      ...REMOTE_COMMANDS.map((c) => c.key),
      // PluginView.ts: `error` is a ref typed `'plugin_unavailable' |
      // 'plugin_contract_mismatch' | null`, never a literal passed to
      // `t(...)` (it is `t(error.value)`).
      'plugin_unavailable',
      'plugin_contract_mismatch',
      // SystemView.vue: the success message of `waitForReturn` arrives as a
      // parameter (`t.value(successKey)`), the key being chosen by the
      // confirmed action. Both literals do exist in the file, but at the call
      // site and not inside a `t(...)` — so the regex does not see them.
      'system_restarted',
      'system_device_restarted',
      // components/links.ts: the labels of the listening platforms are
      // indexed by `platform` (`t(LINK_LABEL[link.platform])`), hence never
      // hard-coded in a `t(...)` call — the regex does not see them, this
      // explicit addition is what covers them.
      ...Object.values(LINK_LABEL),
    ])

    const missing = [...used].filter((key) => !catalog.has(key)).sort()
    expect(missing).toEqual([])
  })
})
