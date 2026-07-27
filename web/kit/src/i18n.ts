export type Catalog = Record<string, string>

/// Résolution d'une clé puis interpolation des jetons `{nom}`, en miroir de
/// ce que fait le Rust (`catalog.get(key)` puis `str::replace("{n}", …)`).
/// Clé absente : on renvoie la clé elle-même, exactement comme
/// `ritornello_i18n::Catalog::get`. Un jeton dont la valeur n'est pas fournie
/// reste tel quel, plutôt que de disparaître : un texte visiblement incomplet
/// est plus facile à diagnostiquer qu'un texte silencieusement tronqué.
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
