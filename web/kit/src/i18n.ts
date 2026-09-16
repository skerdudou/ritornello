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
/// instead of `"hello {toString}"`. No catalog key triggers this today,
/// which is exactly why it would otherwise have sat here.
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
export function createT(catalog: Catalog) {
  return (key: string, params?: Record<string, string | number>): string => {
    const template = catalog[key] ?? key
    return params ? interpolate(template, params) : template
  }
}
