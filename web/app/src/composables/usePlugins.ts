import { api } from '@ritornello/ui'
import { computed, ref } from 'vue'
import type { StatusPayload } from '../types'

/**
 * The state of the plugins, at **module** level — a single set of state for
 * the whole SPA, like the catalog of `useCatalog` and the metrics of
 * `useMetrics`.
 *
 * This is not tidying but the fix of a defect. This state lived in two
 * copies: a local `ref` in `App.vue` for the menu entries of the admin pages,
 * another in `ConfigView.vue` for the table. The navigation one was read
 * **once only**, when the SPA was mounted, and never again. Two symptoms
 * followed, which were really one:
 *
 * - disabling a plugin left its entry in the top menu, and a click led to an
 *   admin page that no longer existed (the core removes the backend, the route
 *   answers 404);
 * - re-enabling it left the row on "stalled" indefinitely, whereas the core
 *   replaces it as soon as the plugin announces itself. An F5 was the only
 *   remedy, for both.
 *
 * The menu entries are therefore derived from the same source as the table,
 * and that source re-reads itself — see `watch`.
 */

const state = ref<StatusPayload>({ plugins: [], active_source: '' })

/**
 * `/api/status` unreachable. Distinguished from an empty state: a navigation
 * without any admin plugin is the hardest symptom to attribute, the page
 * looking normal otherwise.
 */
const unavailable = ref(false)

/**
 * Re-read period while a plugin is "stalled".
 *
 * A probe, and not a pushed stream: the core exposes no SSE on the state of
 * the plugins, and adding one for a window of a few seconds would cost more
 * than it brings. What makes probing acceptable on a Raspberry Pi 2 is that
 * it **does not run in steady state**: it only arms itself as long as a row
 * says "stalled", that is, during the window between the launch of a binary
 * and its announcement.
 */
const PERIOD_MS = 1500

/**
 * Cap on re-reads, i.e. 30 s.
 *
 * Without it, a launched plugin that **never announces itself** — a state the
 * core names itself (`Gathered::figes`) and which describes a faulty plugin,
 * not a slow one — would make the page probe until it is closed. The core
 * only leaves 5 s to a connection to write its announcement line; 30 s thus
 * cover the process launch with a wide margin, and beyond that the "stalled"
 * row is no longer a wait but a diagnosis, which must stay displayed as is.
 */
const MAX_ATTEMPTS = 20

let timer: ReturnType<typeof setTimeout> | null = null
let remaining = 0

/** Names of the reachable plugins that have an admin page, deduplicated.
 *
 * One status row per (name, kind): a multi-kind plugin with an admin page
 * (e.g. `mpd` as `input` + `display`) pushes several rows carrying the same
 * `admin: true`. Without the `Set`, the nav would display as many identical
 * links as kinds — see the same `${name}-${kind}` key in `ConfigView.vue` for
 * the table, which for its part must keep the duplicates.
 *
 * No filter on `disabled` nor on `stalled`, and this is not an oversight: the
 * core replaces **all** the rows of a disabled plugin by a single
 * `disabled()`, and those of a relaunched plugin by a `genre_inconnu()`, both
 * of which carry `admin: false`. An `admin: true` therefore proves on its own
 * that the plugin has announced itself and that its backend is wired. Adding
 * `&& !p.disabled` would be a guard whose falseness would never show.
 */
const admins = computed(() => [
  ...new Set(state.value.plugins.filter((p) => p.admin).map((p) => p.name)),
])

/** Is there a launched plugin that has not spoken yet?
 *
 * **Both states**, and that is the trap of this re-read: since a freshly
 * re-enabled plugin is reported "starting" and no longer "stalled", watching
 * only `stalled` would have disarmed the probing during exactly the window it
 * exists for — the one where the row is going to be replaced by the
 * announcement. Re-enabling would have become invisible without F5 again, the
 * former defect.
 */
const pending = () => state.value.plugins.some((p) => p.stalled || p.starting)

/**
 * Re-reads `/api/status`. On failure, the previous state is **kept**: a
 * transient outage must not empty the menu nor the table.
 */
async function reload(): Promise<void> {
  const s = await api.get<StatusPayload>('/api/status').catch((e) => {
    console.warn('GET /api/status unavailable: navigation without the admin plugins', e)
    return null
  })
  unavailable.value = s === null
  if (s) state.value = s
}

/**
 * Arms the re-read as long as a plugin is stalled.
 *
 * Called after every toggle and after every re-read. The timer is at module
 * level and reset on every arming: two callers — the nav and the settings
 * page — cannot run two concurrent loops on the same data.
 *
 * The counter starts over at full on every call **coming from outside** (a
 * toggle), and only decreases on the turns of the loop. That is what makes a
 * faulty plugin eventually stop being probed, while a second click by the
 * user always gives the watch another chance.
 */
function watch(): void {
  if (timer !== null) {
    clearTimeout(timer)
    timer = null
  }
  remaining = MAX_ATTEMPTS
  loop()
}

/**
 * Disarms the probing in progress, if there is one.
 *
 * The state of this module is **shared** (at module level, so that two
 * components see the same watch), and an armed probe thus outlives whoever
 * triggered it. In service that is intended. In tests, it is a leak: a
 * `reload()` still in flight at the end of a test consumes a
 * `mockResolvedValueOnce` of the next test — the stubbed `fetch` being global —
 * and shifts its whole sequence of responses. Hence this explicit exit, to be
 * called in an `afterEach`.
 *
 * Also useful the day the UI wants to stop probing without being unmounted.
 */
export function stopped(): void {
  if (timer !== null) {
    clearTimeout(timer)
    timer = null
  }
  remaining = 0
}

function loop(): void {
  if (!pending() || remaining <= 0) {
    timer = null
    return
  }
  remaining -= 1
  timer = setTimeout(async () => {
    await reload()
    loop()
  }, PERIOD_MS)
}

/**
 * Re-reads the state, then watches the "stalled" window that a re-enabling
 * has just opened. This is the single entry point: the bootstrap of the SPA
 * and the post-toggle refresh do exactly the same thing, and giving them two
 * names would have described only an intention, not a difference.
 *
 * Unlike `useMetrics().start()`, several callers are safe: `watch` disarms the
 * running timer before setting another one, so two loops cannot fight over
 * the same data. That is what lets `App.vue` bootstrap and `ConfigView`
 * refresh without coordinating.
 */
async function refresh(): Promise<void> {
  await reload()
  watch()
}

/**
 * No reset export for the tests: this state lives at module level, and the
 * tests start over from a fresh module through `vi.resetModules()` — the same
 * pattern as `useCatalog.test.ts`. An exported `_reset()` would be production
 * code existing for the tests alone, and a second way of emptying the state
 * that would have to be kept in agreement with this one.
 */
export function usePlugins() {
  return { state, unavailable, admins, refresh }
}
