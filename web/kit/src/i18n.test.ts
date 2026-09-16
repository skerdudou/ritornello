import { describe, expect, it } from 'vitest'
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
