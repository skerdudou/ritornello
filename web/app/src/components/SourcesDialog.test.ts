import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { resetCatalog, useCatalog } from '../composables/useCatalog'
import type { SourceRow } from '../types'
import SourcesDialog from './SourcesDialog.vue'

const CATALOG = {
  update_sources_title: 'Update sources',
  update_sources_description: 'The repositories this appliance reads.',
  update_sources_unreadable: 'The list of sources could not be read.',
  update_source_official: 'This is the official repository, which is always read.',
  update_source_announced_by: 'Announced by {plugins}',
  update_source_not_queryable: 'Cannot be checked: not a GitHub repository.',
  update_source_not_checked: 'Not checked yet.',
  update_source_unanswered: 'Did not answer at the last check.',
  update_source_publishes_nothing: 'Publishes nothing for this appliance.',
  update_source_published_plugins: 'Plugins published: {plugins}',
  update_source_published_languages: 'Languages published: {languages}',
  update_source_add_label: 'Repository to add (owner/repo)',
  update_source_add: 'Add',
  update_source_remove: 'Remove',
  update_source_failed: 'The change could not be made. Try again.',
}

const OFFICIAL: SourceRow = {
  repo: 'skerdudou/ritornello', kind: 'official', announced_by: [], queryable: true, stored: false,
  report: { answered: true, plugins: ['console'], languages: ['fr'] },
}
const ANNOUNCED_ONLY: SourceRow = {
  repo: 'ann/only', kind: 'announced', announced_by: ['zed'], queryable: true, stored: false, report: null,
}
const ANNOUNCED_STORED: SourceRow = {
  repo: 'ann/stored', kind: 'announced', announced_by: ['zed', 'yak'], queryable: true, stored: true,
  report: { answered: false, plugins: [], languages: [] },
}
const ADDED: SourceRow = {
  repo: 'me/mine', kind: 'added', announced_by: [], queryable: true, stored: true,
  report: { answered: true, plugins: [], languages: [] },
}

let served: SourceRow[]
/** Every request the fake core received, in order, as `METHOD url body`. */
let calls: string[]
/** What `POST /api/update/sources` answers with for the current test. */
let postAnswer: { status: number; body: string }
let deleteAnswer: { status: number; body: string }

beforeEach(async () => {
  resetCatalog()
  served = [OFFICIAL, ANNOUNCED_ONLY, ANNOUNCED_STORED, ADDED]
  calls = []
  postAnswer = { status: 204, body: '' }
  deleteAnswer = { status: 204, body: '' }
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      if (url === '/api/i18n') return new Response(JSON.stringify(CATALOG), { status: 200 })
      const method = init?.method ?? 'GET'
      calls.push(`${method} ${url}${init?.body ? ` ${init.body}` : ''}`)
      if (url === '/api/update/sources' && method === 'GET') {
        return new Response(JSON.stringify(served), { status: 200 })
      }
      if (url === '/api/update/sources' && method === 'POST') {
        if (postAnswer.status === 0) throw new TypeError('Failed to fetch')
        return new Response(postAnswer.body || null, { status: postAnswer.status })
      }
      if (url.startsWith('/api/update/sources/') && method === 'DELETE') {
        return new Response(deleteAnswer.body || null, { status: deleteAnswer.status })
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

// The content is teleported (`DialogPortal`) into `document.body`.
async function mountDialog() {
  const w = mount(SourcesDialog, { props: { open: true }, attachTo: document.body })
  await flushPromises()
  return w
}

function row(repo: string): HTMLElement {
  const el = document.body.querySelector<HTMLElement>(`[data-source-row][data-repo="${repo}"]`)
  if (!el) throw new Error(`no row for ${repo}`)
  return el
}

const removeButton = (repo: string) => row(repo).querySelector('[data-source-remove]')

describe('SourcesDialog', () => {
  it('lists the official repository first, read-only, with no remove button', async () => {
    await mountDialog()
    const rows = Array.from(document.body.querySelectorAll('[data-source-row]'))
    expect(rows[0]!.getAttribute('data-repo')).toBe('skerdudou/ritornello')
    expect(rows[0]!.textContent).toContain('This is the official repository, which is always read.')
    expect(removeButton('skerdudou/ritornello')).toBeNull()
  })

  it('never offers to remove the official row, even if the core said it was stored', async () => {
    // The two operands of the remove condition are tested apart: this row
    // satisfies `stored` and fails only on `kind`.
    served = [{ ...OFFICIAL, stored: true }]
    await mountDialog()
    expect(removeButton('skerdudou/ritornello')).toBeNull()
  })

  it('gives an announced-only repository no remove button, and names who announced it', async () => {
    await mountDialog()
    expect(row('ann/only').textContent).toContain('Announced by zed')
    expect(removeButton('ann/only')).toBeNull()
  })

  it('gives an announced repository the operator also stored a remove button', async () => {
    await mountDialog()
    expect(row('ann/stored').textContent).toContain('Announced by zed, yak')
    expect(removeButton('ann/stored')).not.toBeNull()
  })

  it('gives an added repository a remove button that deletes it and re-fetches', async () => {
    await mountDialog()
    ;(removeButton('me/mine') as HTMLElement).click()
    await flushPromises()
    expect(calls).toEqual([
      'GET /api/update/sources',
      'DELETE /api/update/sources/me/mine',
      'GET /api/update/sources',
    ])
  })

  it('says a row nobody has checked is not checked, never that it is up to date', async () => {
    await mountDialog()
    expect(row('ann/only').querySelector('[data-source-report]')!.textContent).toBe('Not checked yet.')
  })

  it('tells an unanswered source from one that answered with nothing', async () => {
    await mountDialog()
    expect(row('ann/stored').querySelector('[data-source-report]')!.textContent).toBe(
      'Did not answer at the last check.',
    )
    expect(row('me/mine').querySelector('[data-source-report]')!.textContent).toBe(
      'Publishes nothing for this appliance.',
    )
  })

  it('names the plugins and languages a source published', async () => {
    await mountDialog()
    // Two separate lines, never one run-on sentence.
    const lines = Array.from(row('skerdudou/ritornello').querySelectorAll('[data-source-report]')).map((e) => e.textContent)
    expect(lines).toEqual(['Plugins published: console', 'Languages published: fr'])
  })

  it('says a repository that cannot be queried is not queryable', async () => {
    served = [OFFICIAL, { ...ANNOUNCED_ONLY, repo: 'https://example.org/x', queryable: false }]
    await mountDialog()
    expect(row('https://example.org/x').querySelector('[data-source-not-queryable]')).not.toBeNull()
    expect(row('skerdudou/ritornello').querySelector('[data-source-not-queryable]')).toBeNull()
  })

  it('posts the trimmed input, clears it and re-fetches', async () => {
    await mountDialog()
    const input = document.body.querySelector<HTMLInputElement>('[data-source-input]')!
    input.value = '  Some/Repo  '
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-source-add]')!.click()
    await flushPromises()
    expect(calls).toEqual([
      'GET /api/update/sources',
      'POST /api/update/sources {"repo":"Some/Repo"}',
      'GET /api/update/sources',
    ])
    expect(input.value).toBe('')
  })

  it('shows the 409 error text inline, keeps the input and does not re-fetch', async () => {
    postAnswer = { status: 409, body: JSON.stringify({ error: 'This repository is already in the list.' }) }
    await mountDialog()
    const input = document.body.querySelector<HTMLInputElement>('[data-source-input]')!
    input.value = 'me/mine'
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-source-add]')!.click()
    await flushPromises()
    expect(document.body.querySelector('[data-source-error]')!.textContent).toBe(
      'This repository is already in the list.',
    )
    expect(input.value).toBe('me/mine')
    expect(calls.filter((c) => c === 'GET /api/update/sources')).toHaveLength(1)
  })

  it('shows a generic message when a full channel answers 429 without a body', async () => {
    postAnswer = { status: 429, body: '' }
    await mountDialog()
    const input = document.body.querySelector<HTMLInputElement>('[data-source-input]')!
    input.value = 'a/b'
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-source-add]')!.click()
    await flushPromises()
    expect(document.body.querySelector('[data-source-error]')!.textContent).toBe(
      'The change could not be made. Try again.',
    )
  })

  it('shows a generic message, not the browser text, when the request never reached the core', async () => {
    postAnswer = { status: 0, body: '' }
    await mountDialog()
    const input = document.body.querySelector<HTMLInputElement>('[data-source-input]')!
    input.value = 'a/b'
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-source-add]')!.click()
    await flushPromises()
    expect(document.body.querySelector('[data-source-error]')!.textContent).toBe(
      'The change could not be made. Try again.',
    )
  })

  it('shows a generic message when a removal answers 500 without a body', async () => {
    deleteAnswer = { status: 500, body: '' }
    await mountDialog()
    ;(removeButton('me/mine') as HTMLElement).click()
    await flushPromises()
    expect(document.body.querySelector('[data-source-error]')!.textContent).toBe(
      'The change could not be made. Try again.',
    )
  })

  it('says the list could not be read when the first fetch fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) =>
        url === '/api/i18n'
          ? new Response(JSON.stringify(CATALOG), { status: 200 })
          : new Response('', { status: 500 }),
      ),
    )
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    await mountDialog()
    expect(document.body.querySelector('[data-sources-unreadable]')).not.toBeNull()
  })
})
