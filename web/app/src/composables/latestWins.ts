/**
 * Runs `run` one value at a time, newest value last.
 *
 * **Why.** Two requests sent in parallel may reach the core in any order, and
 * the device would stay on the value that was overtaken. So at most one run is
 * in flight; while it runs, only the newest value pushed is kept (older waiting
 * ones are dropped, since the newest supersedes them), which also bounds the
 * load however fast values arrive. When the run settles, resolved or rejected,
 * the waiting value goes next.
 *
 * `send` in the views never rejects, but this helper does not rely on it: a
 * rejection must not stall the queue.
 */
export function latestWins<T>(run: (value: T) => Promise<unknown>): (value: T) => void {
  let running = false
  let pending: { value: T } | null = null

  const start = (value: T): void => {
    running = true
    const next = (): void => {
      running = false
      if (pending) {
        const { value: v } = pending
        pending = null
        start(v)
      }
    }
    let p: Promise<unknown>
    try {
      p = run(value)
    } catch (e) {
      p = Promise.reject(e)
    }
    p.then(next, next)
  }

  return (value: T): void => {
    if (running) pending = { value }
    else start(value)
  }
}
