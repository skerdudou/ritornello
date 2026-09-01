const JSON_HEADERS = { 'content-type': 'application/json' }

/**
 * Message of a failed response: the `error` field of the JSON body when there
 * is one, `HTTP <code>` otherwise.
 *
 * Shared by `send` and `get`, and it is the fix for a measured defect: only
 * `send` read the body, so a 502 from the core — which does carry its cause —
 * showed up as "HTTP 502" when loading a page, whereas the same failure on a
 * PUT said what was wrong.
 */
async function errorMessage(r: Response): Promise<string> {
  try {
    const j = (await r.json()) as { error?: string }
    if (j && typeof j.error === 'string') return j.error
  } catch {
    // non-JSON body: fall back to the status code
  }
  return `HTTP ${r.status}`
}

/// Returns `null` if the operation is accepted, otherwise the error message —
/// the `error` field of a 422's JSON body when it is there, `HTTP <code>`
/// otherwise. Convention taken as is from the `put()` helper of the current
/// pages, so that migrated views do not have to change their logic.
async function send(method: 'PUT' | 'POST', url: string, body: unknown): Promise<string | null> {
  // A rejection of `fetch` itself (core restarting, Wi-Fi down) is part of the
  // "return value" convention, like the non-ok statuses: nearly all callers do
  // not wrap an `api.put` in a `try`, and an exception here became a silent
  // *unhandled rejection* — the user pressed "Play" or "Save" and nothing
  // happened, with no toast nor message.
  let r: Response
  try {
    r = await fetch(url, { method, headers: JSON_HEADERS, body: JSON.stringify(body) })
  } catch (e) {
    return e instanceof Error ? e.message : String(e)
  }
  if (r.ok) return null
  return errorMessage(r)
}

export const api = {
  // Optional `init`: `useMetrics.ts` uses it to pass an `AbortSignal` per probe,
  // without which a period change could not cancel a request that has become
  // obsolete.
  async get<T>(url: string, init?: RequestInit): Promise<T> {
    const r = await fetch(url, init)
    if (!r.ok) throw new Error(await errorMessage(r))
    return (await r.json()) as T
  },
  put: (url: string, body: unknown) => send('PUT', url, body),
  post: (url: string, body: unknown) => send('POST', url, body),
}
