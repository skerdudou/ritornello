export type Catalog = Record<string, string>

/// Resolution of a key then interpolation of the `{name}` tokens, mirroring
/// what the Rust does (`catalog.get(key)` then `str::replace("{n}", …)`).
/// Missing key: the key itself is returned, exactly like
/// `ritornello_i18n::Catalog::get`. A token whose value is not provided stays
/// as is, rather than disappearing: a visibly incomplete text is easier to
/// diagnose than a silently truncated one.
export function createT(catalog: Catalog) {
  return (key: string, params?: Record<string, string | number>): string => {
    let out = catalog[key] ?? key
    if (params) {
      for (const [name, value] of Object.entries(params)) {
        out = out.replaceAll(`{${name}}`, String(value))
      }
    }
    return out
  }
}
