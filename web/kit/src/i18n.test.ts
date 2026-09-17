import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createT, interpolate } from './i18n'

describe('createT', () => {
  it('resolves a present key', () => {
    const t = createT({ saved: 'Saved' })
    expect(t('saved')).toBe('Saved')
  })

  it('falls back to the key itself when it is missing', () => {
    const t = createT({})
    expect(t('unknown')).toBe('unknown')
  })

  it('interpolates named tokens the way the Rust does', () => {
    const t = createT({ bad_request: 'Invalid request: {detail}' })
    expect(t('bad_request', { detail: 'duplicate preset' })).toBe(
      'Invalid request: duplicate preset',
    )
  })

  it('interpolates a numeric token and leaves unprovided tokens intact', () => {
    const t = createT({ msg: '{n} of {total}' })
    expect(t('msg', { n: 3 })).toBe('3 of {total}')
  })

  it('does not interpret the value: a straight apostrophe passes through as is', () => {
    // This is precisely what the old `{{key}}` substitution broke (Critical
    // defect of dbfa771): here the value is data, never source, so no
    // character is dangerous.
    const t = createT({ hint: "you haven't picked a device yet" })
    expect(t('hint')).toBe("you haven't picked a device yet")
  })

  /// A review round ran `createT({})('toString')` against the version of
  /// `createT` that looked a key up with `catalog[key] ?? key` (bracket
  /// access) and got the native `Object.prototype.toString` **function
  /// object** back, not the key. `Chain::get("toString")` on the Rust side
  /// has always returned `"toString"`, since a plain `HashMap` has no
  /// prototype to walk — the exact divergence `interpolate`'s own
  /// `Object.hasOwn` switch (see its doc, above) was already fixed against
  /// one function up, and had sat unfixed here.
  ///
  /// [MUTATION]: replace `Object.hasOwn(catalog, key) ? catalog[key] : key`
  /// with `catalog[key] ?? key` — this test fails, returning a function
  /// instead of the string `"toString"`.
  it('falls back to the bare key for a key named after an Object.prototype member', () => {
    const t = createT({})
    expect(t('toString')).toBe('toString')
    expect(t('constructor')).toBe('constructor')
  })

  it('still resolves an own key that happens to share a name with a prototype member', () => {
    const t = createT({ toString: 'Custom text' })
    expect(t('toString')).toBe('Custom text')
  })
})

describe('interpolate', () => {
  it('leaves a template with no tokens untouched', () => {
    expect(interpolate('no tokens here', {})).toBe('no tokens here')
  })

  it('leaves an unmatched open brace as is', () => {
    expect(interpolate('broken {token', { token: 'x' })).toBe('broken {token')
  })

  it('never rescans a value for further tokens', () => {
    // A value is data, not a second template: a station literally named
    // "{url}" must reach the output unexamined.
    expect(interpolate('{name}', { name: '{url}' })).toBe('{url}')
  })

  /// The barrier this task exists to build: `"{a} and {b}"` with
  /// `a = "{b}"` and `b = "{a}"`. The correct answer is `"{b} and {a}"`.
  /// A chained `.replaceAll()` fold gets this wrong under **either**
  /// visiting order:
  /// - `a` first: `"{a} and {b}"` → `"{b} and {b}"` → (the `b` pass
  ///   rewrites both) → `"{a} and {a}"`
  /// - `b` first: `"{a} and {b}"` → `"{a} and {a}"` → (the `a` pass
  ///   rewrites both) → `"{b} and {b}"`
  ///
  /// `Object.entries` on a plain object is insertion-ordered in JS, so this
  /// does not depend on an engine's hash-map iteration order — it is a
  /// deterministic proof, not a coin flip.
  ///
  /// [MUTATION]: replace the body of `interpolate` with
  /// `Object.entries(params).reduce((acc, [name, value]) =>
  /// acc.replaceAll(`{${name}}`, String(value)), template)` — this test
  /// fails regardless of key order, for the reason above.
  it('is not affected by parameter order (chained replaceAll gets this wrong either way)', () => {
    expect(interpolate('{a} and {b}', { a: '{b}', b: '{a}' })).toBe('{b} and {a}')
  })

  /// F-2 review round: `name in params` walks the **prototype chain**, so a
  /// token named after an `Object.prototype` member (`toString`,
  /// `constructor`, `valueOf`, `hasOwnProperty`, ...) resolved against that
  /// inherited method instead of staying visible like any other unsupplied
  /// token. No catalog key triggers this today, which is exactly why it
  /// would have sat there undetected. `Object.hasOwn` only sees the
  /// object's own keys, matching the Rust side's plain `HashMap` lookup
  /// (no prototype at all).
  ///
  /// [MUTATION]: replace `Object.hasOwn(params, name)` with `name in
  /// params` — this test fails, because `{}` still has `toString` through
  /// `Object.prototype`.
  it('leaves a token named after an Object.prototype member visible when unsupplied', () => {
    expect(interpolate('hello {toString}', {})).toBe('hello {toString}')
    expect(interpolate('{constructor} and {valueOf}', {})).toBe('{constructor} and {valueOf}')
  })
})

// The couture task 15 exists to prove: the core (for status texts) and the
// browser (for every page) are two consumers of one stacked chain, never
// two independently-behaving chains. There is no way, in this repository's
// toolchain, to invoke the Rust resolver from a vitest run or the reverse
// (cargo lives only in WSL, node only outside it), so the proof runs
// against a **shared fixture** on disk rather than a live cross-call:
// `crates/ritornello-i18n/tests/fixtures/resolver_parity.json`, read here
// exactly as `crates/ritornello-i18n/src/chain.rs`'s
// `resolver_parity_fixture_agrees_between_get_plus_interpolate_and_the_browser`
// reads it on the Rust side.
//
// A first version of this pair mirrored the fixture's cases by hand, one
// literal catalog/key/params/expected per side, cross-referenced only in a
// comment. A review round changed the Rust side's behaviour for an
// unsupplied token and updated only the Rust expectation: both suites
// stayed green while the two resolvers actually disagreed — a comment
// naming the other file enforces nothing. Reading the **same file** from
// both sides is what makes a one-sided edit visible: it either updates the
// shared fixture (and the other side's run then judges the new
// expectation too) or it does not, and the other side's run is against
// the unedited case.
describe('resolver parity with the Rust chain', () => {
  interface ParityCase {
    name: string
    catalog: Record<string, string>
    key: string
    params: Record<string, string>
    expected: string
  }

  const fixturePath = join(
    dirname(fileURLToPath(import.meta.url)),
    '../../../crates/ritornello-i18n/tests/fixtures/resolver_parity.json',
  )
  const cases: ParityCase[] = JSON.parse(readFileSync(fixturePath, 'utf-8'))

  it('loads the shared fixture, and it is not empty', () => {
    // Mirrors the Rust side's own `assert!(cases.len() >= 6, ...)`: a
    // fixture that failed to load or was emptied by accident must fail
    // loudly here too, not read as "every case passed" because there was
    // nothing to iterate.
    expect(cases.length).toBeGreaterThanOrEqual(6)
  })

  for (const c of cases) {
    it(`matches the Rust chain: ${c.name}`, () => {
      const t = createT(c.catalog)
      expect(t(c.key, c.params)).toBe(c.expected)
    })
  }
})
