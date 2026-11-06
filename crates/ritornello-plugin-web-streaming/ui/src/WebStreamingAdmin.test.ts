import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Entry, Job, ProbeLine } from './data'
import {
  BASE, CATALOG, cleanup, clickDialog, inDialog, LIST_URL, mountAdmin, OTHER_URL, server, state, typeDialog, VIDEO_URL,
} from './harness'
import WebStreamingAdmin from './WebStreamingAdmin.vue'

const video = (preset: number, title = `Video ${preset}`): Entry => ({
  preset, url: VIDEO_URL, kind: 'video', title, channel: 'Chan', count: null,
})
const list = (preset: number): Entry => ({
  preset, url: LIST_URL, kind: 'playlist', title: `List ${preset}`, channel: null, count: 12,
})

function titles(w: Awaited<ReturnType<typeof mountAdmin>>['w']): string[] {
  return w.findAll('[data-entry-title]').map((i) => (i.element as HTMLInputElement).value)
}

describe('WebStreamingAdmin', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(() => {
    vi.useRealTimers()
    cleanup()
  })

  // ---- the presets ----------------------------------------------------------

  it('lists the presets in order, and saves them numbered by position', async () => {
    const { w, s } = await mountAdmin({ entries: [video(2, 'B'), video(1, 'A'), list(3)] })
    expect(titles(w)).toEqual(['A', 'B', 'List 3'])
    expect(w.text()).toContain('12 videos')

    await w.findAll('[data-entry-delete]')[0]!.trigger('click')
    await w.findAll('[data-entry-up]')[1]!.trigger('click')
    expect(titles(w)).toEqual(['List 3', 'B'])
    await w.find('[data-save]').trigger('click')
    await flushPromises()

    const [save] = s.putsOf('save') as { entries: Entry[] }[]
    expect(save!.entries.map((e) => [e.preset, e.title, e.kind])).toEqual([
      [1, 'List 3', 'playlist'],
      [2, 'B', 'video'],
    ])
    expect(w.text()).toContain(CATALOG.saved)
  })

  it('shows a refused save with its cause, and keeps what was edited', async () => {
    const { w, s } = await mountAdmin({ entries: [video(1)] })
    s.refusal = '99 presets at most'
    await w.find('[data-entry-title]').setValue('Renamed')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    expect(w.text()).toContain('Could not save: 99 presets at most')
    expect(titles(w)).toEqual(['Renamed'])
  })

  // ---- adding links ---------------------------------------------------------

  it('sends the pasted lines as one probe, and shows one error line per refused link', async () => {
    const results: ProbeLine[] = [
      { line: VIDEO_URL, state: 'found', choices: [{ url: VIDEO_URL, kind: 'video', title: 'Me at the zoo', channel: 'jawed', count: null, site: 'Youtube', identity: null }] },
      { line: 'not a link', state: 'error', code: 'not_a_url' },
      { line: 'https://www.youtube.com/live/jNQXAC9IVRw', state: 'error', code: 'live_stream' },
      { line: 'https://studio.youtube.com/x', state: 'error', code: 'foreign_host', detail: 'studio.youtube.com' },
      { line: 'http://192.168.1.1/x', state: 'error', code: 'local_network', detail: '192.168.1.1' },
    ]
    const { w, s } = await mountAdmin()
    s.onPut = (p) => {
      if (p.op === 'probe') s.data.probe = { running: false, results }
    }
    await w.find('[data-links]').setValue(`${VIDEO_URL}\nnot a link\n\nhttps://www.youtube.com/live/jNQXAC9IVRw\nhttps://studio.youtube.com/x\nhttp://192.168.1.1/x`)
    await w.find('[data-probe]').trigger('click')
    await flushPromises()

    expect(s.putsOf('probe')).toHaveLength(1)
    expect((s.putsOf('probe')[0] as { text: string }).text).toContain('not a link')
    const errors = w.findAll('[data-probe-error]').map((e) => e.text())
    expect(errors).toHaveLength(4)
    expect(errors[0]).toContain('not a link')
    expect(errors[0]).toContain('this is not a link')
    expect(errors[1]).toContain('live streams are not supported')
    expect(errors[2]).toContain('not a supported link (studio.youtube.com)')
    expect(errors[3]).toContain('links to the local network are refused (192.168.1.1)')
    expect(w.findAll('[data-add]')).toHaveLength(1)
  })

  it('adds a probed video as a new unsaved row, once', async () => {
    const found = { url: OTHER_URL, kind: 'video' as const, title: 'Me at the zoo', channel: 'jawed', count: null, site: 'Youtube', identity: null }
    const { w } = await mountAdmin({
      entries: [video(1, 'First')],
      probe: { running: false, results: [{ line: OTHER_URL, state: 'found', choices: [found] }] },
    })
    await w.find('[data-add]').trigger('click')
    expect(titles(w)).toEqual(['First', 'Me at the zoo'])
    expect(w.text()).toContain(CATALOG.unsaved_changes)
    expect(w.find('[data-add]').attributes('disabled')).toBeDefined()
  })

  it('shows the site of a preset, and saves its site and identity with it', async () => {
    const track = 'https://soundcloud.com/forss/flickermood'
    const found = {
      url: track, kind: 'video' as const, title: 'Flickermood', channel: 'Forss', count: null,
      site: 'Soundcloud', identity: 'Soundcloud:293',
    }
    const { w, s } = await mountAdmin({
      entries: [{ ...list(1), site: 'BandcampAlbum' }],
      probe: { running: false, results: [{ line: track, state: 'found', choices: [found] }] },
    })
    expect(w.find('[data-probe-line]').text()).toContain('Flickermood (Soundcloud · video)')
    await w.find('[data-add]').trigger('click')
    const details = w.findAll('[data-entry-detail]').map((d) => d.text())
    expect(details[0]).toMatch(/^BandcampAlbum\s+·\s+playlist/)
    expect(details[1]).toMatch(/^Soundcloud\s+·\s+video\s+·\s+Forss/)
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const [save] = s.putsOf('save') as { entries: Entry[] }[]
    expect(save!.entries.map((e) => [e.site, e.identity])).toEqual([
      ['BandcampAlbum', null],
      ['Soundcloud', 'Soundcloud:293'],
    ])
  })

  it('shows no site for a preset saved without one', async () => {
    const { w } = await mountAdmin({ entries: [video(1)] })
    expect(w.find('[data-entry-site]').exists()).toBe(false)
    expect(w.find('[data-entry-detail]').text()).toMatch(/^video/)
  })

  it('asks which one when a link names a video and its playlist', async () => {
    const choices = [
      { url: VIDEO_URL, kind: 'video' as const, title: 'The video', channel: null, count: null, site: 'Youtube', identity: null },
      { url: LIST_URL, kind: 'playlist' as const, title: 'The playlist', channel: null, count: 12, site: 'YoutubeTab', identity: null },
    ]
    const { w } = await mountAdmin({
      probe: { running: false, results: [{ line: `${VIDEO_URL}&list=PLx`, state: 'found', choices }] },
    })
    expect(w.text()).toContain(CATALOG.choose_one)
    expect(w.find('[data-add]').exists()).toBe(false)
    await w.find('[data-add-choice="playlist"]').trigger('click')
    expect(titles(w)).toEqual(['The playlist'])
    expect(w.findAll('[data-entry-row]')).toHaveLength(1)
  })

  it('refuses a hundredth preset', async () => {
    const entries = Array.from({ length: 99 }, (_, i) => video(i + 1))
    const found = { url: OTHER_URL, kind: 'video' as const, title: 'One too many', channel: null, count: null, site: null, identity: null }
    const { w } = await mountAdmin({
      entries,
      probe: { running: false, results: [{ line: OTHER_URL, state: 'found', choices: [found] }] },
    })
    await w.find('[data-add]').trigger('click')
    expect(w.findAll('[data-entry-row]')).toHaveLength(99)
    expect(w.text()).toContain(CATALOG.limit_reached)
  })

  // ---- downloading ----------------------------------------------------------

  it('greys "Download" out with a reason when no share can receive a download', async () => {
    const { w } = await mountAdmin({ entries: [video(1)], targets: [] })
    await w.find('[data-entry-select]').setValue(true)
    expect(w.find('[data-download]').attributes('disabled')).toBeDefined()
    expect(w.text()).toContain(CATALOG.no_writable_share)
  })

  it('needs a selection and saved changes before it opens the dialog', async () => {
    const { w } = await mountAdmin({ entries: [video(1), video(2)], targets: ['nas'] })
    expect(w.find('[data-download]').attributes('disabled')).toBeDefined()
    await w.findAll('[data-entry-select]')[1]!.setValue(true)
    expect(w.find('[data-download]').attributes('disabled')).toBeUndefined()
    await w.find('[data-entry-title]').setValue('edited')
    expect(w.find('[data-download]').attributes('disabled')).toBeDefined()
    expect(w.text()).toContain(CATALOG.save_first)
  })

  it('offers the shares, preselects the last target, and downloads the selected presets there', async () => {
    const { w, s } = await mountAdmin({
      entries: [video(1), video(2), video(3)],
      targets: ['nas', 'other'],
      last_target: { root: 'nas', path: 'rock' },
      default_names: { 1: 'Video 1', 2: 'Video 2', 3: 'Video 3' },
    })
    await w.findAll('[data-entry-select]')[0]!.setValue(true)
    await w.findAll('[data-entry-select]')[2]!.setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()

    // The remembered folder is listed at once.
    expect(s.putsOf('list_dirs')).toEqual([{ op: 'list_dirs', target: { root: 'nas', path: 'rock' } }])
    expect(inDialog('[data-share="nas"]')).not.toBeNull()
    expect(inDialog('[data-share="other"]')).not.toBeNull()
    await clickDialog('[data-next]')
    await clickDialog('[data-download-here]')

    expect(s.putsOf('download')).toEqual([
      {
        op: 'download', presets: [1, 3], target: { root: 'nas', path: 'rock' },
        format: 'original', cover: 'separate', names: { 1: 'Video 1', 3: 'Video 3' },
      },
    ])
  })

  it('walks down the folders, makes a new one, and says what `$` does', async () => {
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: ['nas'] })
    s.onPut = (p) => {
      if (p.op === 'list_dirs' || p.op === 'create_dir') {
        const target = p.target as { root: string; path: string }
        const created = p.op === 'create_dir' ? [String(p.name)] : []
        s.data.explore = {
          busy: false,
          error: null,
          listing: { target, names: target.path === '' ? ['rock', 'jazz', ...created] : created },
        }
      }
    }
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    // Several shares would be offered; with one, it is already chosen.
    expect(s.putsOf('list_dirs')[0]).toEqual({ op: 'list_dirs', target: { root: 'nas', path: '' } })
    expect(inDialog('[data-dir="rock"]')).not.toBeNull()
    expect(inDialog('[data-dollar-note]')!.textContent).toContain('$')

    await clickDialog('[data-dir="rock"]')
    expect(s.putsOf('list_dirs')[1]).toEqual({ op: 'list_dirs', target: { root: 'nas', path: 'rock' } })
    await typeDialog('[data-new-folder]', 'live')
    await clickDialog('[data-create-folder]')
    expect(s.putsOf('create_dir')).toEqual([
      { op: 'create_dir', target: { root: 'nas', path: 'rock' }, name: 'live' },
    ])
    // Confirmed (its parent lists it): the dialog goes into it.
    expect(s.putsOf('list_dirs')[2]).toEqual({ op: 'list_dirs', target: { root: 'nas', path: 'rock/live' } })
    expect(inDialog('[data-where]')!.textContent).toContain('nas/rock/live')

    await clickDialog('[data-up]')
    expect(s.putsOf('list_dirs')[3]).toEqual({ op: 'list_dirs', target: { root: 'nas', path: 'rock' } })
  })

  it('enters a new folder only once the plugin confirms it, and not when it refuses', async () => {
    vi.useFakeTimers()
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: ['nas'] })
    const parent = { root: 'nas', path: '' }
    s.onPut = (p) => {
      if (p.op === 'list_dirs') s.data.explore = { busy: false, error: null, listing: { target: p.target as typeof parent, names: ['rock'] } }
      // Being made: the plugin still lists the parent as it was.
      if (p.op === 'create_dir') s.data.explore = { busy: true, error: null, listing: { target: parent, names: ['rock'] } }
    }
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await vi.advanceTimersByTimeAsync(10)
    await typeDialog('[data-new-folder]', 'live')
    await clickDialog('[data-create-folder]')
    expect(s.putsOf('list_dirs'), 'not before it exists').toHaveLength(1)
    expect(inDialog('[data-where]')!.textContent).not.toContain('live')

    // Made: the next read lists it in its parent.
    s.data.explore = { busy: false, error: null, listing: { target: parent, names: ['live', 'rock'] } }
    await vi.advanceTimersByTimeAsync(1100)
    expect(s.putsOf('list_dirs')[1]).toEqual({ op: 'list_dirs', target: { root: 'nas', path: 'live' } })

    // Refused (it already exists, say): the dialog stays where it is.
    await clickDialog('[data-up]')
    await vi.advanceTimersByTimeAsync(10)
    s.onPut = (p) => {
      if (p.op === 'list_dirs') s.data.explore = { busy: false, error: null, listing: { target: p.target as typeof parent, names: ['live', 'rock'] } }
      if (p.op === 'create_dir') s.data.explore = { busy: false, error: { code: 'io', detail: 'File exists (os error 17)' }, listing: null }
    }
    const before = s.putsOf('list_dirs').length
    await typeDialog('[data-new-folder]', 'rock')
    await clickDialog('[data-create-folder]')
    await vi.advanceTimersByTimeAsync(3100)
    expect(s.putsOf('list_dirs')).toHaveLength(before)
    expect(inDialog('[data-explore-error]')!.textContent).toContain('File exists')

    // Answered with a listing that does not hold it: not entered either.
    s.onPut = (p) => {
      if (p.op === 'create_dir') s.data.explore = { busy: false, error: null, listing: { target: parent, names: ['live', 'rock'] } }
    }
    await typeDialog('[data-new-folder]', 'jazz')
    await clickDialog('[data-create-folder]')
    await vi.advanceTimersByTimeAsync(3100)
    expect(s.putsOf('list_dirs')).toHaveLength(before)
  })

  it('offers the formats, the last one chosen, and mp3 only with ffmpeg', async () => {
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: ['nas'], last_format: 'm4a', last_cover: 'embedded' })
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    await clickDialog('[data-next]')
    // The last format is the one chosen: its help says so.
    expect(inDialog('[data-format-help]')!.textContent).toContain(CATALOG.format_m4a_help)
    expect((inDialog('[data-format="mp3"]') as HTMLButtonElement).disabled).toBe(false)
    expect(inDialog('[data-mp3-unavailable]')).toBeNull()
    await clickDialog('[data-format="mp3"]')
    await clickDialog('[data-download-here]')
    expect(s.putsOf('download')[0]).toMatchObject({ format: 'mp3', cover: 'embedded' })
  })

  it('does not offer mp3 without ffmpeg, says the command, and falls back from a remembered mp3', async () => {
    const caps = { ...state().capabilities, ffmpeg: null, ffmpeg_hint: 'sudo apt install ffmpeg' }
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: ['nas'], last_format: 'mp3', capabilities: caps })
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    await clickDialog('[data-next]')
    expect((inDialog('[data-format="mp3"]') as HTMLButtonElement).disabled).toBe(true)
    expect(inDialog('[data-mp3-unavailable]')!.textContent).toContain('sudo apt install ffmpeg')
    await clickDialog('[data-download-here]')
    expect(s.putsOf('download')[0]).toMatchObject({ format: 'original' })
  })

  it('asks where each track\'s cover goes, for a playlist too, and says what the folder gets', async () => {
    // The regression: a playlist alone was not asked, its tracks got no
    // picture of their own.
    const { w, s } = await mountAdmin({ entries: [list(1), video(2)], targets: ['nas'], last_cover: 'separate' })
    await w.findAll('[data-entry-select]')[0]!.setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    await clickDialog('[data-next]')
    expect(inDialog('[data-cover-help]')!.textContent).toContain(CATALOG.cover_help_playlist)
    await clickDialog('[data-cover="embedded"]')
    expect(inDialog('[data-cover="embedded"]')!.getAttribute('aria-pressed')).toBe('true')
    await clickDialog('[data-download-here]')
    expect(s.putsOf('download')[0]).toMatchObject({ presets: [1], cover: 'embedded' })
    cleanup()
    const again = await mountAdmin({ entries: [list(1), video(2)], targets: ['nas'] })
    await again.w.findAll('[data-entry-select]')[1]!.setValue(true)
    await again.w.find('[data-download]').trigger('click')
    await flushPromises()
    await clickDialog('[data-next]')
    expect(inDialog('[data-cover-help]')!.textContent).toContain(CATALOG.cover_help)
    expect(inDialog('[data-cover-help]')!.textContent).not.toContain('cover.jpg')
  })

  it('separates where from how: two steps, the destination in sight, nothing lost going back', async () => {
    const { w, s } = await mountAdmin({
      entries: [video(1)],
      targets: ['nas', 'other'],
      last_target: { root: 'nas', path: 'rock' },
    })
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    // Step 1: the browser, no option, no download yet.
    expect(inDialog('[data-step-where]')).not.toBeNull()
    expect(inDialog('[data-format="m4a"]')).toBeNull()
    expect(inDialog('[data-download-here]')).toBeNull()
    expect(inDialog('[data-step]')!.textContent).toContain(CATALOG.dlg_step_where)

    await clickDialog('[data-next]')
    expect(inDialog('[data-step-where]')).toBeNull()
    expect(inDialog('[data-destination]')!.textContent).toContain('nas/rock')
    await clickDialog('[data-format="m4a"]')
    // Back to the folders, another share, and forward again: the format stays.
    await clickDialog('[data-change-target]')
    await clickDialog('[data-share="other"]')
    await clickDialog('[data-next]')
    expect(inDialog('[data-destination]')!.textContent).toContain('other')
    expect(inDialog('[data-format="m4a"]')!.getAttribute('aria-pressed')).toBe('true')
    await clickDialog('[data-back]')
    expect(inDialog('[data-step-where]')).not.toBeNull()
    await clickDialog('[data-next]')
    await clickDialog('[data-download-here]')
    expect(s.putsOf('download')[0]).toMatchObject({ target: { root: 'other', path: '' }, format: 'm4a' })

    // Opened again: on the first step.
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    expect(inDialog('[data-step-where]')).not.toBeNull()
  })

  it('prefills each name with the plugin\'s and sends what was typed', async () => {
    // The plugin's names, not the titles: the page computes none itself.
    const { w, s } = await mountAdmin({
      entries: [video(1, 'Me at the zoo?'), list(2)],
      targets: ['nas'],
      default_names: { 1: 'Me at the zoo_', 2: 'List 2' },
    })
    await w.findAll('[data-entry-select]')[0]!.setValue(true)
    await w.findAll('[data-entry-select]')[1]!.setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    await clickDialog('[data-next]')
    expect((inDialog('[data-name="1"]') as HTMLInputElement).value).toBe('Me at the zoo_')
    expect((inDialog('[data-name="2"]') as HTMLInputElement).placeholder).toBe(CATALOG.name_playlist_hint)
    await typeDialog('[data-name="1"]', 'Zoo, first video')
    await clickDialog('[data-download-here]')
    expect(s.putsOf('download')[0]).toMatchObject({ names: { 1: 'Zoo, first video', 2: 'List 2' } })
  })

  it('says why a share cannot be listed, in the catalog\'s words', async () => {
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: ['nas'] })
    s.onPut = (p) => {
      if (p.op === 'list_dirs') s.data.explore = { busy: false, listing: null, error: { code: 'not_mounted' } }
    }
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    expect(inDialog('[data-explore-error]')!.textContent).toContain('this share is not mounted')
  })

  // ---- the downloads --------------------------------------------------------

  it('says what a row did otherwise than asked, and that it is converting', async () => {
    const job: Job = {
      id: 8,
      presets: [1],
      target: { root: 'nas', path: '' },
      state: 'running',
      rows: [
        { preset: 1, id: 'a', title: 'Kept', state: 'done', notes: [{ code: 'no_ffmpeg' }, { code: 'cover_beside' }] },
        { preset: 1, id: 'b', title: 'Failed conversion', state: 'done', notes: [{ code: 'conversion_failed', detail: 'Unknown encoder' }] },
        { preset: 1, id: 'c', title: 'Turning', state: 'converting' },
        { preset: 1, id: 'd', title: 'There', state: 'done', notes: [{ code: 'already_present' }, { code: 'converted_name_taken' }, { code: 'cover_name_taken' }] },
      ],
    }
    const { w } = await mountAdmin({ jobs: [job] })
    const rows = w.findAll('[data-job-row]').map((r) => r.text())
    expect(rows[0]).toContain(CATALOG.note_no_ffmpeg)
    expect(rows[0]).toContain(CATALOG.note_cover_beside)
    expect(rows[1]).toContain('the conversion failed, kept in the site\'s format: Unknown encoder')
    expect(rows[2]).toContain(CATALOG.state_converting)
    expect(rows[3]).toContain(CATALOG.note_already_present)
    expect(rows[3]).toContain(CATALOG.note_converted_name_taken)
    expect(rows[3]).toContain(CATALOG.note_cover_name_taken)
  })

  it('shows each video of a job with its state and a reason, and cancels', async () => {
    const job: Job = {
      id: 7,
      presets: [1],
      target: { root: 'nas', path: 'rock' },
      state: 'running',
      rows: [
        { preset: 1, id: 'a', title: 'Done one', state: 'done', percent: 100 },
        { preset: 1, id: 'b', title: 'Broken one', state: 'failed', detail: { code: 'failed', detail: 'ERROR: Video unavailable' } },
        { preset: 1, id: 'c', title: 'Skipped one', state: 'skipped', detail: { code: 'unavailable' } },
        { preset: 1, id: 'd', title: 'Going one', state: 'running', percent: 42 },
        { preset: 1, id: 'e', title: 'Cancelled one', state: 'cancelled' },
        { preset: 9, id: '', title: '', state: 'failed', detail: { code: 'no_such_preset' } },
      ],
    }
    const { w, s } = await mountAdmin({ jobs: [job] })
    const rows = w.findAll('[data-job-row]').map((r) => r.text())
    expect(rows[0]).toContain('done')
    expect(rows[1]).toContain('failed')
    // The tool's own line stays raw, behind a translated sentence.
    expect(rows[1]).toContain('yt-dlp: ERROR: Video unavailable')
    expect(rows[2]).toContain('skipped')
    expect(rows[2]).toContain('unavailable')
    expect(rows[5]).toContain('Preset 9')
    expect(rows[5]).toContain('no such preset')
    // The space is there: a lone leading space in a `<template>` once
    // vanished at compile time, and "running42%" was shown.
    expect(rows[3]).toContain(`${CATALOG.state_running} 42%`)
    expect(rows[4]).toContain('cancelled')
    expect(w.find('[data-job]').text()).toContain('Download 7 to nas/rock')
    await w.find('[data-job-cancel]').trigger('click')
    expect(s.putsOf('cancel')).toEqual([{ op: 'cancel', job: 7 }])
  })

  it('offers no cancel for a job that is over', async () => {
    const { w } = await mountAdmin({
      jobs: [{ id: 1, presets: [1], target: { root: 'nas', path: '' }, state: 'failed', detail: { code: 'not_mounted' }, rows: [] }],
    })
    expect(w.find('[data-job-cancel]').exists()).toBe(false)
    expect(w.find('[data-job]').text()).toContain('this share is not mounted')
  })

  it('clears the downloads that are over, and offers it only when there is one', async () => {
    const at = { root: 'nas', path: '' }
    const { w, s } = await mountAdmin({
      jobs: [{ id: 1, presets: [1], target: at, state: 'running', rows: [] }],
    })
    expect(w.find('[data-jobs-clear]').exists(), 'nothing over: nothing to clear').toBe(false)
    s.data.jobs = [
      { id: 1, presets: [1], target: at, state: 'running', rows: [] },
      { id: 2, presets: [1], target: at, state: 'done', rows: [] },
    ]
    await w.find('[data-save]').trigger('click') // any send reads again
    await flushPromises()
    s.onPut = (p) => {
      if (p.op === 'clear_jobs') s.data.jobs = s.data.jobs.filter((j) => j.state === 'running')
    }
    await w.find('[data-jobs-clear]').trigger('click')
    await flushPromises()
    expect(s.putsOf('clear_jobs')).toEqual([{ op: 'clear_jobs' }])
    expect(w.findAll('[data-job]')).toHaveLength(1)
    expect(w.find('[data-jobs-clear]').exists()).toBe(false)
  })

  it('shows the downloads right under the presets, where a download is started', async () => {
    const { w } = await mountAdmin({ entries: [video(1)] })
    const titles = w.findAll('[data-slot="card-title"]').map((c) => c.text())
    expect(titles.slice(0, 2)).toEqual([CATALOG.entries_title, CATALOG.jobs_title])
  })

  // ---- polling --------------------------------------------------------------

  it('polls while work runs and stops when nothing does', async () => {
    vi.useFakeTimers()
    const { s } = await mountAdmin({ probe: { running: true, results: [] }, targets: ['nas'] })
    const afterLoad = s.gets()
    await vi.advanceTimersByTimeAsync(3100)
    expect(s.gets()).toBeGreaterThanOrEqual(afterLoad + 3)

    s.data.probe = { running: false, results: [] }
    await vi.advanceTimersByTimeAsync(1100)
    const settled = s.gets()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(s.gets(), 'nothing runs any more: no request').toBe(settled)
  })

  it('does not poll a page where nothing ever ran', async () => {
    vi.useFakeTimers()
    const { s } = await mountAdmin({ targets: ['nas'] })
    // The one look at the shares asked on mount is followed by one read.
    await vi.advanceTimersByTimeAsync(1500)
    const settled = s.gets()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(s.gets()).toBe(settled)
  })

  it('keeps reading after a failed read while work runs', async () => {
    vi.useFakeTimers()
    const { s } = await mountAdmin({ probe: { running: true, results: [] }, targets: ['nas'] })
    const ok = s.spy.getMockImplementation()!
    s.spy.mockImplementationOnce(async () => {
      throw new Error('plugin restarting')
    })
    await vi.advanceTimersByTimeAsync(1100)
    s.spy.mockImplementation(ok)
    const afterFailure = s.gets()
    await vi.advanceTimersByTimeAsync(3100)
    expect(s.gets(), 'the page tried again').toBeGreaterThan(afterFailure)
    s.data.probe = { running: false, results: [] }
    await vi.advanceTimersByTimeAsync(1100)
    const settled = s.gets()
    await vi.advanceTimersByTimeAsync(5000)
    expect(s.gets()).toBe(settled)
  })

  // ---- the shares are not known for good ------------------------------------

  it('asks the plugin to look at its shares on mount, and again when the dialog opens', async () => {
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: ['nas'] })
    expect(s.putsOf('list_targets')).toHaveLength(1)
    await w.find('[data-entry-select]').setValue(true)
    await w.find('[data-download]').trigger('click')
    await flushPromises()
    expect(s.putsOf('list_targets')).toHaveLength(2)
  })

  it('asks again for the shares every ten seconds while none is offered, and stops once one is', async () => {
    vi.useFakeTimers()
    const { s } = await mountAdmin({ targets: [] })
    expect(s.putsOf('list_targets')).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(10_100)
    expect(s.putsOf('list_targets'), 'the page sits idle and asks again').toHaveLength(2)

    s.data.targets = ['nas']
    await vi.advanceTimersByTimeAsync(10_100)
    expect(s.putsOf('list_targets'), 'a share is there: no more asking').toHaveLength(2)
    await vi.advanceTimersByTimeAsync(30_000)
    expect(s.putsOf('list_targets')).toHaveLength(2)
  })

  it('offers Download once a share shows up after the page loaded', async () => {
    vi.useFakeTimers()
    const { w, s } = await mountAdmin({ entries: [video(1)], targets: [] })
    await w.find('[data-entry-select]').setValue(true)
    expect(w.find('[data-download]').attributes('disabled')).toBeDefined()
    s.data.targets = ['nas']
    await vi.advanceTimersByTimeAsync(1100)
    expect(w.find('[data-download]').attributes('disabled')).toBeUndefined()
    expect(w.text()).not.toContain(CATALOG.no_writable_share)
  })

  // ---- reordering -----------------------------------------------------------

  it('reorders by drag and drop, and the numbers follow the position', async () => {
    const { w, s } = await mountAdmin({ entries: [video(1, 'A'), video(2, 'B'), video(3, 'C')] })
    const rows = () => w.findAll('[data-entry-row]')
    await rows()[0]!.trigger('dragstart')
    await rows()[2]!.trigger('drop')
    expect(titles(w)).toEqual(['B', 'C', 'A'])
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const [save] = s.putsOf('save') as { entries: Entry[] }[]
    expect(save!.entries.map((e) => [e.preset, e.title])).toEqual([[1, 'B'], [2, 'C'], [3, 'A']])
  })

  it('moves a row down with the arrow, and the last row cannot go further', async () => {
    const { w } = await mountAdmin({ entries: [video(1, 'A'), video(2, 'B')] })
    await w.findAll('[data-entry-down]')[0]!.trigger('click')
    expect(titles(w)).toEqual(['B', 'A'])
    expect(w.findAll('[data-entry-down]')[1]!.attributes('disabled')).toBeDefined()
    expect(w.findAll('[data-entry-up]')[0]!.attributes('disabled')).toBeDefined()
  })

  it('starts polling when a probe is sent', async () => {
    vi.useFakeTimers()
    const { w, s } = await mountAdmin()
    s.onPut = (p) => {
      if (p.op === 'probe') s.data.probe = { running: true, results: [{ line: VIDEO_URL, state: 'pending' }] }
    }
    await w.find('[data-links]').setValue(VIDEO_URL)
    await w.find('[data-probe]').trigger('click')
    await flushPromises()
    expect(w.text()).toContain(CATALOG.probing)
    const before = s.gets()
    await vi.advanceTimersByTimeAsync(2100)
    expect(s.gets()).toBeGreaterThan(before)
  })

  // ---- yt-dlp ---------------------------------------------------------------

  it('shows the version and the channel, and switches to nightly', async () => {
    const { w, s } = await mountAdmin()
    expect(w.find('[data-tool-version]').text()).toContain('2026.10.26')
    const box = w.find('[data-nightly]')
    expect((box.element as HTMLInputElement).checked).toBe(false)
    await box.setValue(true)
    expect(s.putsOf('set_channel')).toEqual([{ op: 'set_channel', channel: 'nightly' }])
  })

  it('checks for an update with the guard on, and offers "install anyway" only after a refusal', async () => {
    const { w, s } = await mountAdmin()
    expect(w.find('[data-install-anyway]').exists()).toBe(false)
    await w.find('[data-update]').trigger('click')
    expect(s.putsOf('update_tool')).toEqual([{ op: 'update_tool', force: false }])
  })

  it('says the refusal and installs anyway on request', async () => {
    const { w, s } = await mountAdmin({
      tool: {
        ...state().tool,
        error_code: { code: 'rollback_older', params: { offered: '2025.12.31', installed: '2026.10.26' } },
        forceable: true,
      },
    })
    expect(w.find('[data-tool-error]').text()).toContain('release 2025.12.31 is older than the installed 2026.10.26')
    await w.find('[data-install-anyway]').trigger('click')
    expect(s.putsOf('update_tool')).toEqual([{ op: 'update_tool', force: true }])
  })

  it('shows a failure that no forced update would take without the button', async () => {
    const { w } = await mountAdmin({
      tool: { ...state().tool, error_code: { code: 'refused_bad_signature', params: { detail: 'x' } }, forceable: false },
    })
    expect(w.find('[data-tool-error]').text()).toContain('not signed by yt-dlp')
    expect(w.find('[data-install-anyway]').exists()).toBe(false)
  })

  it('greys the update out while it runs, and says what it waits for', async () => {
    const { w } = await mountAdmin({
      tool: { ...state().tool, updating: true, waiting_for_jobs: true },
    })
    expect(w.find('[data-update]').attributes('disabled')).toBeDefined()
    expect(w.text()).toContain(CATALOG.waiting_for_jobs)
  })

  it('says why the last update failed in the page\'s language, with the raw text behind it', async () => {
    const { w } = await mountAdmin({
      tool: {
        ...state().tool,
        error_code: { code: 'install_swap', params: { detail: 'Permission denied (os error 13)' } },
      },
    })
    expect(w.find('[data-tool-error]').text()).toContain(
      'The last update failed: the new yt-dlp could not be put in place: Permission denied (os error 13)',
    )
  })

  it('says why the plugin\'s own JavaScript runtime could not be installed', async () => {
    const { w } = await mountAdmin({
      tool: { ...state().tool, runtime_error_code: { code: 'fetch', params: { detail: 'connection reset' } } },
    })
    expect(w.find('[data-runtime-error]').text()).toContain('JavaScript runtime: the download failed: connection reset')
  })

  it('says a share error is the share\'s, not yt-dlp\'s', async () => {
    const job: Job = {
      id: 1, presets: [1], target: { root: 'nas', path: '' }, state: 'done',
      rows: [
        { preset: 1, id: 'a', title: 'A', state: 'failed', detail: { code: 'io', detail: 'Permission denied (os error 13)' } },
        { preset: 1, id: 'b', title: 'B', state: 'failed', detail: { code: 'failed_silent' } },
      ],
    }
    const { w } = await mountAdmin({ jobs: [job] })
    const rows = w.findAll('[data-job-row]').map((r) => r.text())
    expect(rows[0]).toContain('the share answered: Permission denied (os error 13)')
    expect(rows[0]).not.toContain('yt-dlp:')
    expect(rows[1]).toContain('yt-dlp stopped without saying why')
  })

  it('says the last playback that failed, why, where and when, with yt-dlp\'s own words', async () => {
    const quiet = await mountAdmin()
    expect(quiet.w.find('[data-last-failure]').exists()).toBe(false)
    cleanup()
    const time = 1_730_000_000
    const { w } = await mountAdmin({
      last_failure: { time, host: 'www.youtube.com', code: 'guard_refused', detail: 'ERROR: local network refused: 10.0.0.1' },
    })
    const text = w.find('[data-last-failure]').text()
    expect(text).toContain(CATALOG.play_failed_guard_refused)
    expect(text).toContain('www.youtube.com')
    expect(text).toContain(new Date(time * 1000).toLocaleString())
    expect(w.find('[data-last-failure-detail]').text()).toContain('yt-dlp said: ERROR: local network refused: 10.0.0.1')
  })

  it('says when a check found nothing new, and not while one runs or after a failure', async () => {
    const time = 1_730_000_000
    const checked = await mountAdmin({ tool: { ...state().tool, last_check: time } })
    expect(checked.w.find('[data-checked]').text()).toContain(`Up to date (checked ${new Date(time * 1000).toLocaleString()})`)
    cleanup()
    const running = await mountAdmin({ tool: { ...state().tool, last_check: time, updating: true } })
    expect(running.w.find('[data-checked]').exists()).toBe(false)
    cleanup()
    const failed = await mountAdmin({
      tool: { ...state().tool, last_check: time, error_code: { code: 'refused_not_listed' } },
    })
    expect(failed.w.find('[data-checked]').exists()).toBe(false)
    cleanup()
    const never = await mountAdmin()
    expect(never.w.find('[data-checked]').exists()).toBe(false)
  })

  it('keeps reading while the first install is pending, and shows it once done', async () => {
    vi.useFakeTimers()
    const { w, s } = await mountAdmin({ tool: { ...state().tool, installed: false, version: null }, targets: ['nas'] })
    expect(w.find('[data-tool-version]').text()).toContain(CATALOG.tool_not_installed)
    // The install starts and ends between two reads.
    s.data.tool = { ...state().tool }
    await vi.advanceTimersByTimeAsync(1100)
    expect(w.find('[data-tool-version]').text()).toContain('2026.10.26')
    // Let the mount's own grace reads run out first: the page asks for the
    // shares (`list_targets`) right after its first read, and that request's
    // few follow-up reads may land after the install is shown — on CI they
    // did, and counting from here read one too many (6 for 5). Five seconds
    // cover the longest grace (3 reads, one a second); the page still
    // reading past that, because work seems to run, is what fails below.
    await vi.advanceTimersByTimeAsync(5_000)
    const settled = s.gets()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(s.gets(), 'installed: no more reading').toBe(settled)
  })

  it('names a download row by its title, "track n" for a nameless track, and the number of a gone preset', async () => {
    const job: Job = {
      id: 1, presets: [3, 2], target: { root: 'nas', path: '' }, state: 'done',
      rows: [
        { preset: 3, id: '', title: '', state: 'failed', detail: { code: 'no_such_preset' } },
        { preset: 2, id: 'a', title: 'First', state: 'done' },
        { preset: 2, id: 'b', title: '', state: 'done' },
      ],
    }
    const { w } = await mountAdmin({ jobs: [job] })
    const titles = w.findAll('[data-job-row-title]').map((r) => r.text())
    // "track 2": its place in its own preset, not in the job.
    expect(titles).toEqual(['Preset 3', 'First', 'track 2'])
  })

  it('says yt-dlp is not installed yet', async () => {
    const { w } = await mountAdmin({
      tool: { ...state().tool, installed: false, version: null },
    })
    expect(w.find('[data-tool-version]').text()).toContain(CATALOG.tool_not_installed)
  })

  // ---- the device -----------------------------------------------------------

  it('tells a missing python3 with the exact command, and asks again on request', async () => {
    const { w, s } = await mountAdmin({
      capabilities: {
        python3: null, js_runtime: null, js_runtime_slow: false,
        apt_hint: 'sudo apt install python3', checking: false, js_hopeless: false, js_runtime_unmaintained: false,
        ffmpeg: null, ffmpeg_hint: 'sudo apt install ffmpeg',
      },
    })
    expect(w.find('[data-python]').text()).toContain('sudo apt install python3')
    // No apt hint for the JavaScript runtime.
    expect(w.find('[data-js]').text()).not.toContain('apt')
    // ffmpeg is optional, and says what its absence costs and how to fix it.
    expect(w.find('[data-ffmpeg]').text()).toBe(
      "ffmpeg is missing: downloads stay in the site's format. Install it with: sudo apt install ffmpeg",
    )
    await w.find('[data-recheck]').trigger('click')
    expect(s.putsOf('reprobe')).toHaveLength(1)
  })

  it('names the ffmpeg found', async () => {
    const { w } = await mountAdmin()
    expect(w.find('[data-ffmpeg]').text()).toBe('ffmpeg 7.1.1-1+b1: downloads can be converted to mp3 or m4a')
  })

  // The whole sentence, with toBe: the two parts were once printed glued
  // together ("…: quickjsQuickJS is…"), which a `toContain` let through. And
  // it says the runtime is only the second attempt's, which is what the
  // wrapper does: a device showing QuickJS still plays without it first.
  it('names the fallback runtime, says QuickJS is slow, and that none helps where none exists', async () => {
    const base = state().capabilities
    const slow = await mountAdmin({ capabilities: { ...base, js_runtime: 'quickjs', js_runtime_slow: true } })
    expect(slow.w.find('[data-js]').text()).toBe(
      'JavaScript runtime, used only when a video fails without one: QuickJS It is slow: about two minutes for such a video.',
    )
    cleanup()
    const none = await mountAdmin({ capabilities: { ...base, js_runtime: null, js_hopeless: true } })
    expect(none.w.find('[data-js]').text()).toBe(CATALOG.js_hopeless)
    cleanup()
    const fine = await mountAdmin()
    expect(fine.w.find('[data-js]').text()).toBe('JavaScript runtime, used only when a video fails without one: Node.js')
  })

  // Past node 22's end of life the runtime line stays, and one sentence says
  // the plugin keeps the last node 22 as it is.
  it('says the plugin\'s own node 22 is no longer maintained, only when the plugin says so', async () => {
    const base = state().capabilities
    const old = await mountAdmin({ capabilities: { ...base, js_runtime_unmaintained: true } })
    expect(old.w.find('[data-js]').text()).toBe(
      'JavaScript runtime, used only when a video fails without one: Node.js '
        + 'Node.js 22 is no longer maintained by its authors: the plugin keeps its last release as it is.',
    )
    cleanup()
    const fine = await mountAdmin()
    expect(fine.w.find('[data-js]').text()).not.toContain(CATALOG.js_unmaintained)
  })

  it('names the runtime a failure is about', async () => {
    const { w } = await mountAdmin({
      tool: {
        ...state().tool,
        runtime_error_code: { code: 'bad_signature', params: { runtime: 'node', detail: 'made by none of the embedded release keys' } },
      },
    })
    expect(w.find('[data-runtime-error]').text()).toBe(
      'JavaScript runtime: the published sums are not signed by a known Node.js release key: made by none of the embedded release keys',
    )
  })

  // ---- loading --------------------------------------------------------------

  it('is inert when the first load failed: nothing can overwrite what it could not read', async () => {
    const s = server()
    s.spy.mockImplementation(async () => new Response(JSON.stringify({ error: 'plugin_timeout' }), { status: 502 }))
    const w = mount(WebStreamingAdmin, { props: { catalog: CATALOG, base: BASE }, attachTo: document.body })
    await flushPromises()
    expect(w.text()).toContain('Error: plugin_timeout')
    expect(w.find('[data-save]').attributes('disabled')).toBeDefined()
    await w.find('[data-save]').trigger('click')
    expect(s.spy.mock.calls.filter((c) => (c[1] as RequestInit | undefined)?.method === 'PUT')).toHaveLength(0)
  })
})
