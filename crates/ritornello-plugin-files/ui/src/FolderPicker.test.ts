// `FolderPicker` is a **leaf** entirely driven by its props: it talks
// neither to the server nor to a dialog. It is therefore mounted directly,
// without the page harness — routing it through `FilesAdmin` would make
// these five assertions depend on the wiring of two wizards which, on their
// own, have their own tests.
import { mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import FolderPicker from './FolderPicker.vue'
import type { Exploration } from './data'
import { CATALOG } from './harness'

const t = createT(CATALOG)

const BASE: Exploration = {
  open: true,
  kind: 'local',
  host: '',
  share: '',
  path: '/media/usb',
  shares: [],
  dirs: ['Albums', 'Live'],
  audioCount: 3,
  busy: false,
  error: null,
}

function mountPicker(overrides: Partial<Exploration> = {}, path = '/media/usb') {
  return mount(FolderPicker, {
    props: { exploration: { ...BASE, ...overrides }, t, frozen: false, path },
  })
}

describe('FolderPicker', () => {
  it('lists the subfolders and announces the audio count', () => {
    // The count is what says we are in the right place: without it a
    // folder is chosen while hoping.
    const w = mountPicker()
    expect(w.findAll('[data-picker-folder]')).toHaveLength(2)
    expect(w.get('[data-audio-count]').text()).toContain('3')
  })

  it('descending emits the folder name, not a path', async () => {
    // It is the caller that knows how to compose the path: a local path and
    // an SMB path are not composed the same way.
    const w = mountPicker()
    await w.findAll('[data-picker-folder]')[1]!.trigger('click')
    expect(w.emitted('descend')?.[0]).toEqual(['Live'])
  })

  it('an empty folder says so instead of displaying nothing', () => {
    // An empty list without a sentence reads like a loading that never finished.
    const w = mountPicker({ dirs: [] })
    expect(w.find('[data-picker-empty]').exists()).toBe(true)
  })

  it('while waiting, nothing is clickable and the wait is visible', () => {
    // Without `disabled`, an impatient double click would stack two
    // requests; without the sentence, the frozen screen would look like a
    // folder that is not responding.
    const w = mountPicker({ busy: true })
    expect(w.find('[data-picker-busy]').exists()).toBe(true)
    expect(w.findAll('[data-picker-folder]')[0]!.attributes('disabled')).toBeDefined()
    expect(w.get('[data-picker-go-up]').attributes('disabled')).toBeDefined()
  })

  it('a refusal displays in place of the tree', () => {
    // Displaying an empty tree under an error message would suggest the
    // folder exists and is empty.
    const w = mountPicker({ error: 'host unreachable' })
    expect(w.get('[data-picker-error]').text()).toContain('host unreachable')
    expect(w.findAll('[data-picker-folder]')).toHaveLength(0)
    expect(w.find('[data-picker-empty]').exists()).toBe(false)
  })

  it('displays the path it is given, not the one from the exploration', () => {
    // Fix for a reported defect: on the share side, `exploration.path` is
    // relative to the share, so the share "vanished" from the path as soon
    // as it was entered. It is the caller that composes the full address.
    const w = mountPicker({ path: 'Yann Tiersen' }, '//192.168.1.15/music/Yann Tiersen')
    expect(w.get('[data-picker-path]').text()).toBe('//192.168.1.15/music/Yann Tiersen')
  })

  it('truncates a path that is too long from the start and keeps the whole in a tooltip', () => {
    // Truncating is there to make it fit, not to hide it: the full value
    // stays reachable on hover.
    const long = '/mnt/c/Users/skerdudou/OneDrive - Klee Group/perso/steven prive/mp3/Muse'
    const w = mountPicker({}, long)
    const span = w.get('[data-picker-path]')
    expect(span.text().startsWith('…/')).toBe(true)
    expect(span.text().endsWith('Muse')).toBe(true)
    expect(span.attributes('title')).toBe(long)
  })
})
