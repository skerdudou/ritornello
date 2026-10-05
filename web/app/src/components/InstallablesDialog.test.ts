import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { ComponentOffer, UpdatePayload } from '../types'
import InstallablesDialog from './InstallablesDialog.vue'

const CATALOG = {
  installables_title: 'Add a plugin',
  installables_description: 'Plugins this release publishes and this appliance does not have.',
  installables_empty: 'Nothing to add: this appliance has everything this release publishes.',
  installables_unknown: 'Not known yet: no usable check has run so far.',
  installables_no_catalogue:
    'This release does not publish a description of its components, or it could not be read; only their names are known.',
  installables_install: 'Install',
  installables_checking: 'Looking for components…',
  installables_queue_busy: 'The appliance is busy, try again in a moment.',
  installables_retry: 'Retry',
  installables_slow: 'Still looking, this is taking longer than usual.',
  update_last_attempt_failed: 'The last attempt failed',
  plugin_privileged_note: 'Privileged component: install or uninstall it with ritornello-install.',
  plugin_kind_display: 'affichage',
  plugin_kind_source: 'source',
  installables_from_repo: 'Third-party plugin from {repo}, version {version}.',
  installables_conflict: 'Offered by several repositories ({repos}): none is trusted.',
}

// The catalogue response the fake `fetch` answers `GET /api/update/catalogue`
// with, for the current test — a plain module-level variable rather than a
// parameter threaded through `beforeEach`, since `stubCatalogue` is called
// from inside individual tests, after the shared stub is already installed.
let catalogueResponse: { components: Record<string, { kinds: string[]; description: string }> } = {
  components: {
    // Deliberately unlike the real console plugin's own description (which
    // carries no "tty" — see `ritornello-plugin-console/Cargo.toml`): this is
    // a fixture for this test file alone, and the assertion below is checked
    // against it, not against production data.
    console: { kinds: ['display'], description: 'A tty or small display.' },
  },
}

/** What each source's own catalogue answers (`?repo=`), by the query string
 *  the dialog sends; a repository absent here answers 404. */
let sourceCatalogueResponses: Record<string, unknown> = {}
const sourceCatalogueAsks: string[] = []

/** Replaces the stubbed `/api/update/catalogue` answer for one test. */
function stubCatalogue(response: { components: Record<string, { kinds: string[]; description: string }> }) {
  catalogueResponse = response
}

// A check the dialog itself runs on opening (follow-up B). What the fake core
// answers `POST /api/update/check` with, and `GET /api/update` afterwards.
const NOW_S = Math.floor(Date.now() / 1000)
let checkStatus = 202
let checkPosts = 0
let served: UpdatePayload

beforeEach(async () => {
  resetCatalog()
  checkStatus = 202
  checkPosts = 0
  sourceCatalogueResponses = {}
  sourceCatalogueAsks.length = 0
  served = {
    outcome: { kind: 'ok' },
    release_version: null,
    release_url: null,
    last_check_unix_s: NOW_S,
    components: [],
    busy: null,
    last_rollback: null,
  }
  // Only the interval is faked: `flushPromises` needs the real timers.
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] })
  stubCatalogue({
    components: { console: { kinds: ['display'], description: 'A tty or small display.' } },
  })
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
      if (url === '/api/update/catalogue') {
        return new Response(JSON.stringify(catalogueResponse), { status: 200 })
      }
      if (url.startsWith('/api/update/catalogue?repo=')) {
        const repo = decodeURIComponent(url.slice('/api/update/catalogue?repo='.length))
        sourceCatalogueAsks.push(repo)
        const answer = sourceCatalogueResponses[repo]
        if (answer === undefined) return new Response('', { status: 404 })
        return new Response(JSON.stringify(answer), { status: 200 })
      }
      if (url === '/api/update/check' && init?.method === 'POST') {
        checkPosts += 1
        return new Response('', { status: checkStatus })
      }
      if (url === '/api/update') return new Response(JSON.stringify(served), { status: 200 })
      return new Response('', { status: 404 })
    }),
  )
  await useCatalog().reload()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

function offer(overrides: Partial<ComponentOffer> & { name: string }): ComponentOffer {
  return {
    kind: 'plugin',
    declared: false,
    binary_present: false,
    installed: null,
    offered: null,
    availability: 'not_installed',
    ...overrides,
  }
}

// The dialog's content is teleported (`DialogPortal`), so it lands in
// `document.body` regardless of where the component is mounted — same
// convention as `UpdateDialog.test.ts` and `CoverCacheDetails.test.ts`.
//
// `outcome` defaults to `ok`: most of this file exercises the ordinary case
// of a check that actually looked, and only the tests about Major C
// (distinguishing "nothing to add" from "cannot know yet") vary it.
function mountDialog(
  components: ComponentOffer[],
  outcome: UpdatePayload['outcome'] = { kind: 'ok' },
  lastCheckUnixS: number | null = NOW_S,
  busy: string | null = null,
) {
  // The dialog decides on what the device answers when it opens, so the
  // device says what the page was handed; a test of a stale page overrides it.
  served = { ...served, outcome, last_check_unix_s: lastCheckUnixS, busy }
  return mount(InstallablesDialog, {
    props: { open: true, components, outcome, lastCheckUnixS, busy },
    attachTo: document.body,
  })
}

/** Mounts once, closes, then reopens with the same components — to check
 * that the catalogue is asked only the first time. */
async function openTwice() {
  const spy = vi.fn(async (url: string) => {
    if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
    if (url === '/api/update/catalogue') {
      return new Response(JSON.stringify(catalogueResponse), { status: 200 })
    }
    return new Response('', { status: 404 })
  })
  vi.stubGlobal('fetch', spy)
  const components = [offer({ name: 'console', availability: 'not_installed' })]
  const w = mountDialog(components)
  await flushPromises()
  await w.setProps({ open: false })
  await w.setProps({ open: true, components })
  await flushPromises()
  return { w, spy }
}

describe('InstallablesDialog', () => {
  it('lists a component the release offers and the device does not have', async () => {
    mountDialog([offer({ name: 'console', availability: 'not_installed', offered: '0.2.0-beta.1' })])
    await flushPromises()
    expect(document.body.querySelectorAll('[data-installable-row]')).toHaveLength(1)
    expect(document.body.querySelector('[data-installable-kind]')?.textContent).toBe('affichage')
    expect(document.body.querySelector('[data-installable-description]')?.textContent?.trim()).toContain('tty')
  })

  it('says so when there is nothing to add, after a check that actually looked', async () => {
    // Distinguished from "the catalogue is missing": one is a device that
    // has everything, the other a release that published no description.
    // `outcome: ok` (the default) is what licenses reading an empty `rows` as
    // completeness rather than silence — see the next test.
    mountDialog([])
    await flushPromises()
    expect(document.body.querySelector('[data-installables-empty]')).not.toBeNull()
    expect(document.body.querySelector('[data-installables-unknown]')).toBeNull()
    expect(document.body.querySelector('[data-installables-no-catalogue]')).toBeNull()
  })

  it('says it cannot know, rather than "nothing to add", when no usable check has run', async () => {
    // Major C: an empty `rows` also happens when the device has never
    // checked, when the last check failed, when there is no release at all,
    // or when only prereleases are published — none of those license "this
    // appliance already has everything this version publishes", which is
    // what the appliance would otherwise claim (and precisely what the e2e
    // harness would see, since it never runs a check).
    const outcomes: UpdatePayload['outcome'][] = [
      { kind: 'never_checked' },
      { kind: 'no_release' },
      { kind: 'only_prereleases' },
      { kind: 'failed', detail: 'boom' },
    ]
    for (const outcome of outcomes) {
      // A `failed` with no timestamp is the never-succeeded kind, and the recheck
      // the dialog runs on opening cannot be queued here either.
      checkStatus = 500
      const w = mountDialog([], outcome, outcome.kind === 'failed' ? null : NOW_S)
      await flushPromises()
      expect(document.body.querySelector('[data-installables-unknown]')).not.toBeNull()
      expect(document.body.querySelector('[data-installables-empty]')).toBeNull()
      w.unmount()
      document.body.innerHTML = ''
    }
  })

  // N3: `failed` is published both for a failed check and for a refused
  // install that followed a successful check (`install_report`, `update/mod.rs`)
  // — in the second case the rows on screen are the real ones a check just
  // built, and `last_check_unix_s` says so. Before this fix `hasUsableCheck`
  // treated every `failed` as silence, so this state showed
  // `installables_unknown` ("no usable check has run so far") on a device
  // that had, in fact, just looked.
  it('says nothing to add, not that it cannot know, when a failed outcome follows a real check', async () => {
    checkStatus = 500 // the recheck on opening cannot be queued: the rows stay as they were
    mountDialog([], { kind: 'failed', detail: 'boom' }, 1_760_000_000)
    await flushPromises()
    expect(document.body.querySelector('[data-installables-empty]')).not.toBeNull()
    expect(document.body.querySelector('[data-installables-unknown]')).toBeNull()
  })

  it('still says it cannot know when a failed outcome has never had a real check behind it', async () => {
    // The `null` fixture default: a first check that fails on a fresh
    // device, distinguished from the case above by `last_check_unix_s`
    // alone. Kept as its own test so a mutant that ignores `lastCheckUnixS`
    // entirely (always usable, or never) cannot survive either assertion.
    checkStatus = 500
    mountDialog([], { kind: 'failed', detail: 'boom' }, null)
    await flushPromises()
    expect(document.body.querySelector('[data-installables-unknown]')).not.toBeNull()
    expect(document.body.querySelector('[data-installables-empty]')).toBeNull()
  })

  // m6: the update card's own Install button already disables itself while
  // `update.busy` is set; this dialog's did not, so a second press during an
  // in-flight install re-enqueued a second `Job::Install` of the same
  // component.
  it('disables Install while a job is running', async () => {
    // The job starts while the dialog is already open (an Install pressed in
    // it): opening on a running job would only wait, see the checks below.
    const w = mountDialog([offer({ name: 'console', availability: 'not_installed' })])
    await flushPromises()
    expect(
      document.body.querySelector<HTMLButtonElement>('[data-installable-install]')?.disabled,
    ).toBe(false)
    await w.setProps({ busy: 'Installing console…' })
    await flushPromises()
    expect(
      document.body.querySelector<HTMLButtonElement>('[data-installable-install]')?.disabled,
    ).toBe(true)
  })

  it('stays usable when the release publishes no catalogue', async () => {
    // Every release published before this chantier is in that case,
    // `v0.2.0-beta.1` included. Names alone, and the dialog says why the
    // rest is missing — a device does not have to know more.
    stubCatalogue({ components: {} })
    mountDialog([offer({ name: 'console', availability: 'not_installed' })])
    await flushPromises()
    expect(document.body.querySelectorAll('[data-installable-row]')).toHaveLength(1)
    expect(document.body.querySelector('[data-installable-description]')).toBeNull()
    expect(document.body.querySelector('[data-installables-no-catalogue]')).not.toBeNull()
    expect(
      document.body.querySelector('[data-installable-install]')?.getAttribute('disabled'),
    ).toBeNull()
  })

  it('retries the catalogue on a later opening after the page itself could not reach it', async () => {
    // Distinguished from "this release has no catalogue": here the request
    // from the page to the core failed outright, and that must not be
    // remembered for the rest of the page's life the way a genuine empty
    // answer is. Before the fix, `asked` stayed latched after the failure and
    // the second opening never asked again.
    let calls = 0
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => {
        if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
        if (url === '/api/update/catalogue') {
          calls += 1
          if (calls === 1) return new Response('', { status: 500 })
          return new Response(JSON.stringify(catalogueResponse), { status: 200 })
        }
        return new Response('', { status: 404 })
      }),
    )
    const components = [offer({ name: 'console', availability: 'not_installed' })]
    const w = mountDialog(components)
    await flushPromises()
    expect(calls).toBe(1)
    expect(document.body.querySelector('[data-installable-description]')).toBeNull()

    await w.setProps({ open: false })
    await w.setProps({ open: true })
    await flushPromises()

    expect(calls).toBe(2)
    expect(document.body.querySelector('[data-installable-description]')?.textContent).toContain('tty')
  })

  it('asks the catalogue once, on opening, and not again on the second opening', async () => {
    // The core caches it for the session; the dialog must not defeat that
    // by re-asking on every mount.
    const { spy } = await openTwice()
    expect(spy.mock.calls.filter((c) => String(c[0]).includes('/api/update/catalogue'))).toHaveLength(1)
  })

  // Arbitration C19: a language pack never rides this dialog's Install
  // button, because that button calls `installPlugin()` — a bare `POST
  // /api/update/install`, and `carries()` refuses `Offer::LanguagePack`
  // definitively. Both assertions, not only the first: a mutant that hides
  // every `not_installed` row (language pack or not) would still pass a
  // test that only checked the pack's absence.
  it('never lists a language pack, even one the device does not have, and still lists a plugin in the same state', async () => {
    mountDialog([
      offer({ name: 'ritornello-lang-fr', kind: 'language_pack', availability: 'not_installed', offered: '0.2.1' }),
      offer({ name: 'console', kind: 'plugin', availability: 'not_installed', offered: '0.2.1' }),
    ])
    await flushPromises()
    expect(document.body.querySelector('[data-installable-row][data-name="ritornello-lang-fr"]')).toBeNull()
    expect(document.body.querySelector('[data-installable-row][data-name="console"]')).not.toBeNull()
  })

  // Both halves in one test: a privileged plugin's row (`installable: false`,
  // set by the core from its own name, never from an archive read here) loses
  // the Install button and gains the sentence, and an ordinary row on the
  // same dialog keeps its button — a one-sided assertion would pass against a
  // dialog that renders the sentence everywhere, or nowhere.
  it('shows the ritornello-install sentence instead of Install for a privileged plugin, and only that one', async () => {
    mountDialog([
      offer({ name: 'files', availability: 'not_installed', offered: '0.2.1', installable: false }),
      offer({ name: 'console', availability: 'not_installed', offered: '0.2.1' }),
    ])
    await flushPromises()
    const filesRow = document.body.querySelector('[data-installable-row][data-name="files"]')!
    const consoleRow = document.body.querySelector('[data-installable-row][data-name="console"]')!

    expect(filesRow.querySelector('[data-installable-install]')).toBeNull()
    expect(filesRow.querySelector('[data-installable-privileged]')?.textContent).toContain('ritornello-install')

    expect(consoleRow.querySelector('[data-installable-privileged]')).toBeNull()
    expect(consoleRow.querySelector('[data-installable-install]')).not.toBeNull()
  })

  it('emits install for one row at a time', async () => {
    // One button per row rather than a multi-selection: installing a plugin
    // is the rare gesture, and the update dialog's checkbox list exists for
    // the frequent one. Generalise only if use asks for it.
    const w = mountDialog([offer({ name: 'console', availability: 'not_installed' })])
    await flushPromises()
    document.body.querySelector<HTMLButtonElement>('[data-installable-install]')!.click()
    await flushPromises()
    expect(w.emitted('install')).toEqual([['console']])
  })

  describe('a third-party source', () => {
    const fresh = (overrides: Partial<ComponentOffer> = {}) =>
      offer({ name: 'zed', kind: 'third_party', offered: '1.2.0', third_party_repo: 'z/zed', ...overrides })
    const row = (name: string) => document.body.querySelector(`[data-installable-row][data-name="${name}"]`)

    // Spec §6: without a catalogue of its own, a stranger's offer is shown
    // by name, source and version — and its Install does not install: it
    // hands the page the name **and the repository** for the second consent.
    // **[MUTATION]** emit `install` for every row (the first click installs):
    // red here, and the ConfigView wiring test goes red too.
    it('names the repository and version, and asks the page for consent rather than installing', async () => {
      const w = mountDialog([fresh()])
      await flushPromises()
      expect(row('zed')?.querySelector('[data-installable-repo]')?.textContent)
        .toBe('Third-party plugin from z/zed, version 1.2.0.')
      expect(row('zed')?.querySelector('[data-installable-description]')).toBeNull()
      ;(row('zed')!.querySelector('[data-installable-install]') as HTMLElement).click()
      await flushPromises()
      expect(w.emitted('install-third-party')).toEqual([['zed', 'z/zed']])
      expect(w.emitted('install')).toBeUndefined()
    })

    // Both repositories named, and no button at all: a contested name is not
    // installable from anyone. A neighbouring fresh row keeps its button, so
    // a dialog that drops every Install cannot pass.
    // **[MUTATION]** render Install on a conflict row: red.
    it('shows a contested name with every repository and nothing to install', async () => {
      mountDialog([
        offer({ name: 'dup', kind: 'third_party', installable: false, conflict_repos: ['a/one', 'b/two'] }),
        fresh(),
      ])
      await flushPromises()
      const dup = row('dup')!
      expect(dup.querySelector('[data-installable-install]')).toBeNull()
      expect(dup.querySelector('[data-installable-conflict]')?.textContent).toContain('a/one, b/two')
      expect(dup.querySelector('[data-installable-privileged]')).toBeNull()
      expect(dup.querySelector('[data-installable-repo]')).toBeNull()
      expect(row('zed')?.querySelector('[data-installable-install]')).not.toBeNull()
    })

    // Fails closed (fix round 1): a third-party row with neither a source
    // nor a conflict has no one to name in the second consent, so it gets no
    // button at all rather than a direct install. Its fresh neighbour keeps
    // its button, so a dialog that drops every Install cannot pass.
    // **[MUTATION]** Install back to a plain `v-else`: red.
    it('offers no Install for a third-party row that names no source', async () => {
      const w = mountDialog([offer({ name: 'orphan', kind: 'third_party', offered: '1.0.0' }), fresh()])
      await flushPromises()
      expect(row('orphan')).not.toBeNull()
      expect(row('orphan')?.querySelector('[data-installable-install]')).toBeNull()
      expect(row('zed')?.querySelector('[data-installable-install]')).not.toBeNull()
      expect(w.emitted('install')).toBeUndefined()
    })

    // The source's own catalogue describes its own offer — asked from the
    // core by repository, once — and nothing else: its entry for `radio`
    // never describes our `radio`, nor does our catalogue's `zed` describe
    // the stranger's.
    // **[MUTATION]** read a third-party row's entry from our catalogue, or
    // any source's entry by name alone: red.
    it('describes a stranger only from its own catalogue, and never one of our rows with it', async () => {
      stubCatalogue({
        components: {
          radio: { kinds: ['source'], description: 'Ours' },
          zed: { kinds: ['source'], description: 'Our catalogue on their name' },
        },
      })
      sourceCatalogueResponses['z/zed'] = {
        components: {
          zed: { kinds: ['display'], description: 'Zed, by Z' },
          radio: { kinds: ['display'], description: 'Not theirs to say' },
        },
      }
      mountDialog([offer({ name: 'radio', offered: '0.3.0' }), fresh()])
      await flushPromises()
      expect(sourceCatalogueAsks).toEqual(['z/zed'])
      expect(row('zed')?.querySelector('[data-installable-description]')?.textContent?.trim()).toBe('Zed, by Z')
      expect(row('zed')?.querySelector('[data-installable-kind]')?.textContent).toBe('affichage')
      expect(row('radio')?.querySelector('[data-installable-description]')?.textContent?.trim()).toBe('Ours')
    })

    // A list of strangers' offers alone does not blame our release for not
    // describing them.
    it('does not say our release has no catalogue when only strangers are listed', async () => {
      stubCatalogue({ components: {} })
      mountDialog([fresh()])
      await flushPromises()
      expect(document.body.querySelector('[data-installables-no-catalogue]')).toBeNull()
      expect(row('zed')).not.toBeNull()
    })
  })

  // Follow-up B: opening the dialog runs the check itself.
  describe('the check it runs on opening', () => {
    const NEVER: UpdatePayload['outcome'] = { kind: 'never_checked' }
    const q = (sel: string) => document.body.querySelector(sel)

    it('enqueues exactly one check on a never-checked state, shows the spinner and no list, then the rows once it lands', async () => {
      const w = mountDialog([], NEVER, null)
      await flushPromises()
      expect(checkPosts).toBe(1)
      expect(q('[data-check-running]')?.textContent).toContain('Looking for components')
      // Neither answer is given while nobody has looked.
      expect(q('[data-installables-unknown]')).toBeNull()
      expect(q('[data-installables-empty]')).toBeNull()

      // The check lands: the page (ConfigView) reloads and hands the rows down.
      served = {
        ...served,
        outcome: { kind: 'ok' },
        last_check_unix_s: NOW_S,
        components: [offer({ name: 'console', availability: 'not_installed', offered: '0.2.1' })],
      }
      await vi.advanceTimersByTimeAsync(2000)
      await flushPromises()
      expect(w.emitted('refresh')).toHaveLength(1)
      await w.setProps({
        components: served.components,
        outcome: { kind: 'ok' },
        lastCheckUnixS: NOW_S,
      })
      await flushPromises()
      expect(q('[data-check-running]')).toBeNull()
      expect(q('[data-installable-row][data-name="console"]')).not.toBeNull()
      expect(checkPosts).toBe(1)
    })

    it('does not enqueue after a check under an hour old, and enqueues after an older one', async () => {
      mountDialog([], { kind: 'ok' }, NOW_S - 3000)
      await flushPromises()
      expect(checkPosts).toBe(0)
      expect(q('[data-check-running]')).toBeNull()
      document.body.innerHTML = ''

      mountDialog([], { kind: 'ok' }, NOW_S - 4000)
      await flushPromises()
      expect(checkPosts).toBe(1)
      expect(q('[data-check-running]')).not.toBeNull()
    })

    it('only waits when a check is already running: no second enqueue, and the spinner shows', async () => {
      mountDialog([], NEVER, null, 'Checking…')
      await flushPromises()
      expect(checkPosts).toBe(0)
      expect(q('[data-check-running]')).not.toBeNull()
    })

    it('does not enqueue when the device is already running a check the page has not learnt of', async () => {
      // The page's props say idle and never checked (its poll has not read
      // the update card's Check yet); the device says a check is running.
      const w = mountDialog([], NEVER, null)
      w.unmount()
      document.body.innerHTML = ''
      checkPosts = 0
      served = { ...served, busy: 'Checking…' }
      mount(InstallablesDialog, {
        props: { open: true, components: [], outcome: NEVER, lastCheckUnixS: null, busy: null },
        attachTo: document.body,
      })
      await flushPromises()
      expect(checkPosts).toBe(0)
      expect(q('[data-check-running]')).not.toBeNull()
    })

    it('says it is still looking after the ceiling, with a Retry, and keeps the list', async () => {
      const w = mountDialog(
        [offer({ name: 'console', availability: 'not_installed', offered: '0.2.1' })],
        NEVER,
        null,
      )
      await flushPromises()
      await vi.advanceTimersByTimeAsync(2000 * 9)
      expect(q('[data-check-slow]')).toBeNull()
      expect(q('[data-check-running]')).not.toBeNull()
      await vi.advanceTimersByTimeAsync(2000)
      expect(q('[data-check-slow]')?.textContent).toContain('Still looking')
      expect(q('[data-check-retry]')).not.toBeNull()
      expect(q('[data-check-running]')).toBeNull()
      expect(q('[data-installable-row][data-name="console"]')).not.toBeNull()
      // A late landing clears it and tells the page.
      served = { ...served, last_check_unix_s: NOW_S, outcome: { kind: 'ok' } }
      await vi.advanceTimersByTimeAsync(2000)
      await flushPromises()
      expect(q('[data-check-slow]')).toBeNull()
      expect(w.emitted('refresh')).toHaveLength(1)
    })

    it('shows the failure with a Retry that enqueues again', async () => {
      checkStatus = 500
      mountDialog([], NEVER, null)
      await flushPromises()
      expect(checkPosts).toBe(1)
      expect(q('[data-check-failed]')?.textContent).toContain('The last attempt failed')
      expect(q('[data-check-running]')).toBeNull()

      checkStatus = 202
      q('[data-check-retry]')!.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await flushPromises()
      expect(checkPosts).toBe(2)
      expect(q('[data-check-running]')).not.toBeNull()
      expect(q('[data-check-failed]')).toBeNull()
    })

    it('shows "busy, try again" on a 429 and not the failure', async () => {
      checkStatus = 429
      mountDialog([], NEVER, null)
      await flushPromises()
      expect(q('[data-check-queue-full]')?.textContent).toContain('busy')
      expect(q('[data-check-failed]')).toBeNull()
      expect(q('[data-check-running]')).toBeNull()
    })
  })
})
