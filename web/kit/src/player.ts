/**
 * Subscription to the player's state changes, pushed by the core.
 *
 * Lives in the kit — rather than being copied into each plugin — because the
 * plugin pages have the same need as the shell: knowing *when* something has
 * changed in order to re-read what concerns them. Without it, a page that shows
 * the current track can only poll, and the project has already ruled against
 * polling (see the shell's `usePlayer`: "the core already pushes every
 * change").
 *
 * The payload is passed **untyped**, on purpose: its shape belongs to the core
 * and will change without notice. A caller that only needs the signal ignores
 * it; one that needs a specific field (the active source, for instance) reads
 * it at its own risk, without freezing here a type that would lie.
 */
export function onPlayer(callback: (state: unknown) => void): () => void {
  // `EventSource` does not exist everywhere (jsdom under test, old engines): its
  // absence must cost the display's freshness, never the page's rendering.
  if (typeof EventSource === 'undefined') return () => {}

  const stream = new EventSource('/api/player')
  stream.onmessage = (e: MessageEvent) => {
    try {
      callback(JSON.parse(e.data as string))
    } catch {
      // Unreadable frame: the signal still counts, the caller will re-read its
      // own source of truth.
      callback(null)
    }
  }
  // No error handling: `EventSource` reconnects on its own, and closing here
  // would deprive the page of any recovery after a core restart — the most
  // common case being `systemctl restart ritornello`.
  return () => stream.close()
}
