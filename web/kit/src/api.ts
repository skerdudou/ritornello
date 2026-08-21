const JSON_HEADERS = { 'content-type': 'application/json' }

/// Renvoie `null` si l'opération est acceptée, sinon le message d'erreur —
/// le champ `error` du corps JSON d'un 422 quand il est là, `HTTP <code>`
/// sinon. Convention reprise telle quelle du helper `put()` des pages
/// actuelles, pour que les vues migrées n'aient pas à changer de logique.
async function send(method: 'PUT' | 'POST', url: string, body: unknown): Promise<string | null> {
  // Le rejet de `fetch` lui-même (cœur en cours de redémarrage, Wi-Fi coupé)
  // fait partie de la convention « valeur de retour », comme les statuts
  // non-ok : la quasi-totalité des appelants ne mettent pas de `try` autour
  // d'un `api.put`, et une exception ici devenait une *unhandled rejection*
  // muette — l'utilisateur pressait « Lecture » ou « Enregistrer » et rien ne
  // se passait, sans toast ni message.
  let r: Response
  try {
    r = await fetch(url, { method, headers: JSON_HEADERS, body: JSON.stringify(body) })
  } catch (e) {
    return e instanceof Error ? e.message : String(e)
  }
  if (r.ok) return null
  try {
    const j = (await r.json()) as { error?: string }
    if (j && typeof j.error === 'string') return j.error
  } catch {
    // corps non JSON : on retombe sur le code
  }
  return `HTTP ${r.status}`
}

export const api = {
  // `init` optionnel : `useMetriques.ts` s'en sert pour passer un `AbortSignal`
  // par sondage, sans quoi un changement de période ne pourrait pas annuler
  // une requête devenue obsolète.
  async get<T>(url: string, init?: RequestInit): Promise<T> {
    const r = await fetch(url, init)
    if (!r.ok) throw new Error(`HTTP ${r.status}`)
    return (await r.json()) as T
  },
  put: (url: string, body: unknown) => send('PUT', url, body),
  post: (url: string, body: unknown) => send('POST', url, body),
}
