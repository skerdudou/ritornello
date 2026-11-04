// Shared harness of this module's tests.
//
// It lives in `src/` and not in a `*.test.ts` so that several test files can
// import it; nothing in `src/index.ts` reaches it, so it never enters the
// built package.

import { flushPromises, mount } from '@vue/test-utils'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { vi } from 'vitest'
import type { Data } from './data'
import WebStreamingAdmin from './WebStreamingAdmin.vue'

/**
 * Prefix deliberately different from `/plugins/web-streaming/`: the name under which
 * a plugin is served comes from `plugins.toml`, hence from the deployment.
 */
export const BASE = '/plugins/tube/'

/**
 * The **real** English catalog of the plugin (the page is tested against what
 * ships, not against made-up labels), plus the common vocabulary it inherits.
 * A flat `key = "value"` reader: the format of the embedded catalogs.
 */
function readCatalog(path: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const line of readFileSync(path, 'utf8').split(/\r?\n/)) {
    const m = /^([A-Za-z0-9_]+) = "(.*)"$/.exec(line.trim())
    if (m) out[m[1]!] = m[2]!
  }
  return out
}

export const CATALOG: Record<string, string> = {
  ...readCatalog(resolve(process.cwd(), '../../ritornello-i18n/src/locales/common_en.toml')),
  ...readCatalog(resolve(process.cwd(), '../src/locales/en.toml')),
}

export const VIDEO_URL = 'https://www.youtube.com/watch?v=jNQXAC9IVRw'
export const OTHER_URL = 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'
export const LIST_URL = 'https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI'

export function state(partial: Partial<Data> = {}): Data {
  return {
    entries: [],
    probe: { running: false, results: [] },
    targets: [],
    last_target: null,
    explore: { busy: false, listing: null, error: null },
    jobs: [],
    tool: {
      installed: true,
      version: '2026.10.26',
      channel: 'stable',
      updating: false,
      waiting_for_jobs: false,
      error_code: null,
      forceable: false,
      last_check: null,
      runtime_error_code: null,
    },
    capabilities: {
      python3: '3.11.2',
      js_runtime: 'node',
      js_runtime_slow: false,
      apt_hint: null,
      checking: false,
      js_hopeless: false,
      js_runtime_unmaintained: false,
    },
    last_failure: null,
    ...partial,
  }
}

export interface Server {
  spy: ReturnType<typeof vi.fn>
  /** State returned by the next GET. Modifiable by a test between two calls. */
  data: Data
  /** When non-null, every PUT is refused with this sentence, as is. */
  refusal: string | null
  /** Called before the response to an accepted PUT; used to evolve `data`. */
  onPut: (payload: Record<string, unknown>) => void
  puts: () => Record<string, unknown>[]
  putsOf: (op: string) => Record<string, unknown>[]
  gets: () => number
}

/** Stand-in for the plugin: a GET returns `data`, a PUT returns 204 or a 422 refusal. */
export function server(initial: Partial<Data> = {}): Server {
  const s: Server = {
    spy: vi.fn(),
    data: state(initial),
    refusal: null,
    onPut: () => {},
    puts: () =>
      s.spy.mock.calls
        .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
        .map((c) => JSON.parse(String((c[1] as RequestInit).body)) as Record<string, unknown>),
    putsOf: (op) => s.puts().filter((b) => b.op === op),
    gets: () => s.spy.mock.calls.filter((c) => (c[1] as RequestInit | undefined)?.method === undefined).length,
  }
  s.spy.mockImplementation(async (_url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      if (s.refusal !== null) return new Response(JSON.stringify({ error: s.refusal }), { status: 422 })
      s.onPut(JSON.parse(String(init.body)) as Record<string, unknown>)
      return new Response(null, { status: 204 })
    }
    return new Response(JSON.stringify(s.data), { status: 200 })
  })
  vi.stubGlobal('fetch', s.spy)
  return s
}

/**
 * Mounts the page on a simulated server and waits for its first load.
 *
 * `attachTo: document.body` is not decorative: the kit's `Dialog` renders its
 * content through a portal, outside the component tree (see `inDialog`).
 */
export async function mountAdmin(initial: Partial<Data> = {}) {
  const s = server(initial)
  const w = mount(WebStreamingAdmin, { props: { catalog: CATALOG, base: BASE }, attachTo: document.body })
  await flushPromises()
  return { w, s }
}

/** An element of the open dialog (`wrapper.find()` never finds it: portal). */
export function inDialog(selector: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(selector)
}

export async function clickDialog(selector: string): Promise<void> {
  const el = inDialog(selector)
  if (!el) throw new Error(`no element "${selector}" in the dialog`)
  el.click()
  await flushPromises()
}

export async function typeDialog(selector: string, value: string): Promise<void> {
  const el = inDialog(selector) as HTMLInputElement | null
  if (!el) throw new Error(`no field "${selector}" in the dialog`)
  el.value = value
  el.dispatchEvent(new Event('input', { bubbles: true }))
  await flushPromises()
}

/** The portals are not cleaned by unmounting: empty the body between tests. */
export function cleanup(): void {
  document.body.innerHTML = ''
}
