export type Catalog = Record<string, string>

/// Fills `{name}` tokens in `template` from `params`, in one left-to-right
/// pass that walks the template once and never rescans what it has already
/// emitted. Mirrors `ritornello_i18n::interpolate` on the Rust side (see that
/// module's doc for why a pass-per-parameter chained `.replaceAll()` is
/// wrong): a value inserted for one token must never be read back by a later
/// token's substitution, so parameter order cannot change the result.
///
/// Not a template engine: no expressions, no escaping beyond matching a `{`
/// to its next `}`. A token with no entry in `params` is left **visible**
/// (`{missing}` stays `{missing}`) rather than becoming empty — a visibly
/// wrong text is easier to diagnose than a silently missing one, the same
/// contract as `createT`'s key fallback below.
///
/// Looks up each token with `Object.hasOwn`, never the `in` operator:
/// `in` walks the prototype chain, so a token literally named `toString`,
/// `constructor`, `valueOf` or any other `Object.prototype` member would
/// resolve against that inherited method instead of staying visible — a
/// divergence from the Rust side (a plain `HashMap` lookup, no prototype)
/// that a review caught by actually running `interpolate('hello
/// {toString}', {})`, which returned the function's own source text
/// instead of `"hello {toString}"`.
///
/// **The same hazard sat one function down, in `createT`, until a second
/// review round found it live.** `createT` used to look a key up with
/// `catalog[key] ?? key` — bracket access, exactly the same prototype-chain
/// exposure `interpolate` was fixed against here — so `createT({})
/// ('toString')` returned the native `Object.prototype.toString` function
/// object instead of falling back to the key `"toString"`, while the Rust
/// side's `Chain::get("toString")` correctly returns `"toString"`. Not
/// hypothetical: the SPA calls `t()` with runtime-supplied keys (an error
/// code, a plugin or source name), so an operator naming something
/// `toString` was reachable. `createT` below now uses `Object.hasOwn` too,
/// so this file has exactly one prototype-safe lookup discipline, used
/// twice, rather than one fixed site and one still open.
export function interpolate(template: string, params: Record<string, string | number>): string {
  let out = ''
  let rest = template
  for (;;) {
    const open = rest.indexOf('{')
    if (open === -1) {
      out += rest
      return out
    }
    out += rest.slice(0, open)
    const afterOpen = rest.slice(open + 1)
    const close = afterOpen.indexOf('}')
    if (close === -1) {
      out += rest.slice(open)
      return out
    }
    const name = afterOpen.slice(0, close)
    out += Object.hasOwn(params, name) ? String(params[name]) : `{${name}}`
    rest = afterOpen.slice(close + 1)
  }
}

/// Resolution of a key then interpolation of its `{name}` tokens, mirroring
/// what the Rust does (`catalog.get(key)` then `ritornello_i18n::interpolate`).
/// Missing key: the key itself is returned, exactly like
/// `ritornello_i18n::Catalog::get`.
///
/// **`Object.hasOwn`, not bracket access, for the same reason as
/// `interpolate` above.** `catalog[key]` walks the prototype chain: a key
/// named `toString`, `constructor` or `valueOf` — no catalog defines one
/// today, but `t()` is called with runtime-supplied keys (an error code, a
/// plugin or source name) that this project does not fully control —
/// resolved against the inherited `Object.prototype` method instead of
/// falling back to the key, diverging from `Chain::get`'s plain map lookup
/// on the Rust side. Found live, not merely by inspection: a review ran
/// `createT({})('toString')` and got the function object back.
export function createT(catalog: Catalog) {
  return (key: string, params?: Record<string, string | number>): string => {
    // `noUncheckedIndexedAccess` types `catalog[key]` as `string | undefined`
    // on its own; the `!` is safe here specifically because `Object.hasOwn`
    // just confirmed the key is the object's own, not inherited or absent —
    // TypeScript's narrowing does not special-case `hasOwn`, so it has to be
    // asserted rather than inferred.
    const template = Object.hasOwn(catalog, key) ? catalog[key]! : key
    return params ? interpolate(template, params) : template
  }
}
