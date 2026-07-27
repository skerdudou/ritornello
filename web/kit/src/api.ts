const JSON_HEADERS = { 'content-type': 'application/json' }

/// Renvoie `null` si l'opération est acceptée, sinon le message d'erreur —
/// le champ `error` du corps JSON d'un 422 quand il est là, `HTTP <code>`
/// sinon. Convention reprise telle quelle du helper `put()` des pages
/// actuelles, pour que les vues migrées n'aient pas à changer de logique.
async function send(method: 'PUT' | 'POST', url: string, body: unknown): Promise<string | null> {
  const r = await fetch(url, { method, headers: JSON_HEADERS, body: JSON.stringify(body) })
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
  async get<T>(url: string): Promise<T> {
    const r = await fetch(url)
    if (!r.ok) throw new Error(`HTTP ${r.status}`)
    return (await r.json()) as T
  },
  put: (url: string, body: unknown) => send('PUT', url, body),
  post: (url: string, body: unknown) => send('POST', url, body),
}
