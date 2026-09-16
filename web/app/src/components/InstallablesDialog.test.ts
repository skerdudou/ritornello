import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { ComponentOffer, UpdatePayload } from '../types'
import InstallablesDialog from './InstallablesDialog.vue'

const CATALOG = {
  installables_title: 'Add a component',
  installables_description: 'Components this release publishes and this appliance does not have.',
  installables_empty: 'Nothing to add: this appliance has everything this release publishes.',
  installables_unknown: 'Not known yet: no usable check has run so far.',
  installables_no_catalogue:
    'This release does not publish a description of its components, or it could not be read; only their names are known.',
  installables_install: 'Install',
  plugin_kind_display: 'affichage',
  plugin_kind_source: 'source',
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

/** Replaces the stubbed `/api/update/catalogue` answer for one test. */
function stubCatalogue(response: { components: Record<string, { kinds: string[]; description: string }> }) {
  catalogueResponse = response
}

beforeEach(async () => {
  resetCatalog()
  stubCatalogue({
    components: { console: { kinds: ['display'], description: 'A tty or small display.' } },
  })
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
      if (url === '/api/update/catalogue') {
        return new Response(JSON.stringify(catalogueResponse), { status: 200 })
      }
      return new Response('', { status: 404 })
    }),
  )
  await useCatalog().reload()
})

afterEach(() => {
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
) {
  return mount(InstallablesDialog, { props: { open: true, components, outcome }, attachTo: document.body })
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
      const w = mountDialog([], outcome)
      await flushPromises()
      expect(document.body.querySelector('[data-installables-unknown]')).not.toBeNull()
      expect(document.body.querySelector('[data-installables-empty]')).toBeNull()
      w.unmount()
      document.body.innerHTML = ''
    }
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
})
