import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { LanguageBusy, LanguagePackRow, UpdatePayload } from '../types'
import AddLanguageDialog from './AddLanguageDialog.vue'

const CATALOG = {
  languages_add_title: 'Add a language',
  languages_add_description: 'Languages this release publishes and this appliance does not have.',
  languages_add_empty: 'Nothing to add: every language this release publishes is already installed.',
  installables_unknown: 'Not known yet: no usable check has run so far.',
  installables_checking: 'Looking for components…',
  installables_queue_busy: 'The appliance is busy, try again in a moment.',
  installables_retry: 'Retry',
  installables_slow: 'Still looking, this is taking longer than usual.',
  update_last_attempt_failed: 'The last attempt failed',
  language_pack_install: 'Install',
  language_pack_installing: 'Installing {language}…',
}

const NOW_S = Math.floor(Date.now() / 1000)

const PACKS: LanguagePackRow[] = [
  { language: 'fr', installed: '0.2.1', offered: '0.2.1' },
  { language: 'de', installed: null, offered: '0.2.1' },
  { language: 'es', installed: '0.2.0', offered: '0.2.1' },
  { language: 'it', installed: null, offered: '0.2.1' },
]

let checkStatus = 202
let posts = 0

beforeEach(async () => {
  resetCatalog()
  checkStatus = 202
  posts = 0
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] })
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
      if (url === '/api/update/check' && init?.method === 'POST') {
        posts += 1
        return new Response('', { status: checkStatus })
      }
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

function mountDialog(
  over: {
    packs?: LanguagePackRow[]
    outcome?: UpdatePayload['outcome']
    lastCheckUnixS?: number | null
    busy?: string | null
    packBusy?: LanguageBusy | null
  } = {},
) {
  return mount(AddLanguageDialog, {
    props: {
      open: true,
      packs: PACKS,
      outcome: { kind: 'ok' },
      lastCheckUnixS: NOW_S,
      busy: null,
      packBusy: null,
      ...over,
    },
    attachTo: document.body,
  })
}

const q = (sel: string) => document.body.querySelector<HTMLElement>(sel)
const qa = (sel: string) => Array.from(document.body.querySelectorAll<HTMLElement>(sel))

describe('AddLanguageDialog', () => {
  // Both halves: the two offered-and-absent languages are listed, and neither
  // the installed one nor the installed-with-a-newer-offer one is.
  it('lists only the languages that are offered and not installed', async () => {
    mountDialog()
    await flushPromises()
    expect(qa('[data-add-language-row]').map((r) => r.dataset.language)).toEqual(['de', 'it'])
    expect(q('[data-add-language-row][data-language="fr"]')).toBeNull()
    expect(q('[data-add-language-row][data-language="es"]')).toBeNull()
  })

  it('never lists a language that is neither offered nor installed', async () => {
    mountDialog({ packs: [{ language: 'nl', installed: null, offered: null }, ...PACKS] })
    await flushPromises()
    expect(q('[data-add-language-row][data-language="nl"]')).toBeNull()
    expect(qa('[data-add-language-row]')).toHaveLength(2)
  })

  it('emits install with the language code, one language at a time', async () => {
    const w = mountDialog()
    await flushPromises()
    q('[data-pack-install="it"]')!.click()
    await flushPromises()
    expect(w.emitted('install')).toEqual([['it']])
  })

  it('says what is installing on the busy row and disables every Install button', async () => {
    mountDialog({ packBusy: { language: 'de', action: 'install' } })
    await flushPromises()
    expect(q('[data-add-language-row][data-language="de"] [data-pack-busy]')?.textContent).toContain('Installing')
    expect(q('[data-add-language-row][data-language="it"] [data-pack-busy]')).toBeNull()
    expect(qa('[data-pack-install]')).toHaveLength(2)
    expect(qa('[data-pack-install]').every((b) => (b as HTMLButtonElement).disabled)).toBe(true)
  })

  it('leaves Install enabled when nothing is in flight', async () => {
    mountDialog()
    await flushPromises()
    expect(qa('[data-pack-install]')).toHaveLength(2)
    expect(qa('[data-pack-install]').every((b) => !(b as HTMLButtonElement).disabled)).toBe(true)
  })

  it('says nothing is left to add after a check that looked, and not that it cannot know', async () => {
    mountDialog({ packs: [PACKS[0]!] })
    await flushPromises()
    expect(q('[data-add-language-empty]')).not.toBeNull()
    expect(q('[data-add-language-unknown]')).toBeNull()
  })

  it('checks on opening: one enqueue and a spinner, no list, on a never-checked device', async () => {
    mountDialog({ outcome: { kind: 'never_checked' }, lastCheckUnixS: null })
    await flushPromises()
    expect(posts).toBe(1)
    expect(q('[data-check-running]')?.textContent).toContain('Looking for components')
    expect(q('[data-add-language-row]')).toBeNull()
    expect(q('[data-add-language-empty]')).toBeNull()
    expect(q('[data-add-language-unknown]')).toBeNull()
  })

  it('does not check, and shows no spinner, after a recent check', async () => {
    mountDialog()
    await flushPromises()
    expect(posts).toBe(0)
    expect(q('[data-check-running]')).toBeNull()
  })

  it('shows the failure with Retry when the check cannot be queued, and busy on a 429', async () => {
    checkStatus = 500
    const w = mountDialog({ outcome: { kind: 'never_checked' }, lastCheckUnixS: null })
    await flushPromises()
    expect(q('[data-check-failed]')?.textContent).toContain('The last attempt failed')
    expect(q('[data-check-retry]')).not.toBeNull()
    expect(q('[data-check-queue-full]')).toBeNull()
    w.unmount()
    document.body.innerHTML = ''

    checkStatus = 429
    mountDialog({ outcome: { kind: 'never_checked' }, lastCheckUnixS: null })
    await flushPromises()
    expect(q('[data-check-queue-full]')?.textContent).toContain('busy')
    expect(q('[data-check-failed]')).toBeNull()
  })
})
