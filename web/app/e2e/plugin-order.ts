import { expect, type APIRequestContext, type Page } from '@playwright/test'

/**
 * The declared plugin order, read from the real core rather than the page.
 * One entry per **name**, in first-seen (i.e. file) order: a multi-kind
 * plugin is listed once per kind, and would otherwise look like a move that
 * never happened.
 */
export const pluginOrder = (request: APIRequestContext) => async (): Promise<string[]> => {
  const status = (await (await request.get('/api/status')).json()) as { plugins: { name: string }[] }
  return [...new Set(status.plugins.map((p) => p.name))]
}

/**
 * Puts the declared order back through the API, whatever the page did. A
 * reorder rewrites the harness's `plugins.toml`, and the specs share one
 * worker in CI: an order left behind turns a later, unrelated journey red.
 *
 * Restored through the API, not the page: by the time this runs, the page may
 * be the thing that failed. A gesture's own move may still be in flight when
 * this starts, so:
 *  - given the page, it first waits for the plugins table to unlock (a drop
 *    locks every handle until its answer settles). Not fatal: a page that
 *    failed may never unlock, and the error that matters is the test's own;
 *  - the whole pass is retried, and only accepted once the order has stayed
 *    put for a moment, so a move that lands after a pass read the order is
 *    undone by the next pass rather than left behind.
 */
export async function restoreOrder(request: APIRequestContext, before: string[], page?: Page) {
  if (page) {
    await expect(page.locator('[data-plugins-table] [data-plugin-drag-handle].cursor-not-allowed'))
      .toHaveCount(0)
      .catch(() => {})
  }
  const order = pluginOrder(request)
  await expect(async () => {
    for (const [to, name] of before.entries()) {
      const current = await order()
      if (current[to] !== name) await request.post(`/api/plugins/${name}/move`, { data: { to } })
    }
    expect(await order()).toEqual(before)
    await new Promise((r) => setTimeout(r, 500))
    expect(await order()).toEqual(before)
  }).toPass({ timeout: 15_000 })
}
