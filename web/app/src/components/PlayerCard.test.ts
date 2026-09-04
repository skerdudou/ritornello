import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import PlayerCard from './PlayerCard.vue'
import type { PlayerPayload } from '../types'

// The brand color: the acknowledged exception to the "no hard-coded color"
// rule (owner's decision, see docs/interface.md § Player card). Checking that
// it is indeed there, platform by platform, documents the exception as much as
// it proves it.
const ICON_COLOR = {
  youtube: '#FF0000',
  deezer: '#A238FF',
  apple_music: '#FA243C',
} as const

// jsdom does not provide ResizeObserver; reka-ui uses it to measure the
// ProgressBar slider's track, mounted here as soon as `seekable` is true
// (see web/kit/src/index.test.ts and ProgressBar.test.ts).
beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
})

/**
 * Full state from a fragment: the component receives the state as a prop —
 * HomeView is what holds the page's single SSE connection (the real stream is
 * covered by the HomeView tests and the e2e journey).
 */
function full(state: Partial<PlayerPayload>): PlayerPayload {
  return {
    source: 'radio',
    volume: 60,
    muted: false,
    standby: false,
    preset: null,
    preset_count: null,
    preset_name: null,
    status: null,
    overlay: null,
    artist: null,
    title: null,
    album: null,
    year: null,
    duration_s: null,
    origin: null,
    cover_href: null,
    cover_origin: null,
    position_s: null,
    seekable: false,
    can_eject: false,
    ...state,
  }
}

function mountWith(state: Partial<PlayerPayload> | null) {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(PlayerCard, { props: { state: state ? full(state) : null, seekStep: 10 } })
}

describe('PlayerCard', () => {
  it('shows the source from the first frame', () => {
    const w = mountWith({ source: 'cd' })
    expect(w.get('[data-source]').text()).toBe('cd')
  })

  it('names the absence of a source instead of showing a blank', () => {
    // The core now starts without any source (a slow plugin may announce
    // itself much later), and the protocol says this absence with the empty
    // string. Without a label, one read "Active source:" followed by nothing —
    // a display one takes for a UI failure. The raw key is enough here: the
    // catalog is not loaded under test, `createT` returns the key.
    const w = mountWith({ source: '' })
    expect(w.find('[data-source]').text()).toBe('no_source')
  })

  it('does not say "no source" before the first frame', () => {
    // `state` at `null` means "the state has not arrived yet" and not "there
    // is no source": announcing the absence at that instant would be wrong,
    // and that is the trap of a `||` placed on `state?.source`.
    const w = mountWith(null)
    expect(w.find('[data-source]').text()).toBe('')
  })

  it('shows the current preset when the source declares one', () => {
    const w = mountWith({ preset: 4 })
    expect(w.get('[data-player-preset]').text()).toBe('4')
  })

  it('shows no preset line when the source declares none', () => {
    // `null` covers two situations where there is nothing to number — nothing
    // is playing, or the source does not number (cd without a disc, aux
    // input) — and an empty line there would suggest a failure.
    const w = mountWith({ preset: null })
    expect(w.find('[data-player-preset]').exists()).toBe(false)
  })

  it('shows preset 0 rather than mistaking it for an absence', () => {
    // Guard against a `v-if` written on the value itself: `0` is falsy in
    // JavaScript but remains a declared preset.
    const w = mountWith({ preset: 0 })
    expect(w.find('[data-player-preset]').text()).toBe('0')
  })

  it('adds the preset name when the source declares one', () => {
    const w = mountWith({ preset: 4, preset_name: 'FIP' })
    expect(w.find('[data-player-preset]').text()).toBe('4')
    expect(w.find('[data-player-preset-name]').text()).toBe('FIP')
  })

  it('shows only the number when the source names nothing', () => {
    // The cd case: a declared preset (the track), but no name — no generic
    // i18n key like "station" that would be wrong here.
    const w = mountWith({ preset: 3, preset_name: null })
    expect(w.find('[data-player-preset]').text()).toBe('3')
    expect(w.find('[data-player-preset-name]').exists()).toBe(false)
  })

  it('shows the status declared by the source', () => {
    const w = mountWith({ status: 'PAS DE DISQUE' })
    expect(w.find('[data-player-status]').text()).toBe('PAS DE DISQUE')
  })

  it('shows no status line when there is none', () => {
    const w = mountWith({ status: null })
    expect(w.find('[data-player-status]').exists()).toBe(false)
  })

  it('hides the status line in standby so as not to duplicate the STANDBY badge', () => {
    // The status published in standby is the same word from the same catalog
    // as the "VEILLE" badge shown just above (see M2, branch review): without
    // this masking, the card would show "VEILLE" twice, the second time
    // without a label unlike its neighbours ("Présélection :", "Volume :").
    const w = mountWith({ status: 'VEILLE', standby: true })
    expect(w.find('[data-player-status]').exists()).toBe(false)
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('signals standby', () => {
    const w = mountWith({ standby: true })
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('shows no standby when it is inactive', () => {
    const w = mountWith({ standby: false })
    expect(w.find('[data-standby]').exists()).toBe(false)
  })

  it('shows no track block while nothing is known', () => {
    // Most French stations announce nothing: an empty "Now playing" block
    // would suggest a failure. The player card itself stays.
    const w = mountWith(null)
    expect(w.find('[data-player]').exists()).toBe(true)
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('shows no track block for a duration alone', () => {
    const w = mountWith({ duration_s: 214 })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
  })

  it('adds artist, title, album, duration and origin when they arrive', () => {
    const w = mountWith({
      artist: 'Miles Davis',
      title: 'So What',
      album: 'Kind of Blue',
      duration_s: 545,
      origin: 'musicbrainz',
    })
    expect(w.find('[data-now-playing]').exists()).toBe(true)
    expect(w.find('[data-title]').text()).toBe('So What')
    expect(w.find('[data-artist]').text()).toBe('Miles Davis')
    expect(w.find('[data-album]').text()).toBe('Kind of Blue')
    expect(w.find('[data-duration]').text()).toBe('9:05')
  })

  it('shows a title alone, as the ICY header gives it', () => {
    // ICY delivers a single, unsplit string: it arrives in `title`. The
    // OUI FM webradios even emit it in the order "Title - ARTIST".
    const w = mountWith({ title: 'Made Up - TAHITI 80', origin: 'icy' })
    expect(w.find('[data-title]').text()).toBe('Made Up - TAHITI 80')
    expect(w.find('[data-artist]').exists()).toBe(false)
  })

  it('shows the artist alone when the title is missing', () => {
    // Owner's decision: every available piece of information is shown.
    const w = mountWith({ artist: 'Téléphone', origin: 'ouifm-metas' })
    expect(w.find('[data-artist]').text()).toBe('Téléphone')
    expect(w.find('[data-title]').exists()).toBe(false)
  })

  it('removes the track block when playback stops', async () => {
    // Identity change or stop: the core broadcasts a state without a track,
    // and the old title must not stay on screen — but the player card stays,
    // with the source (the volume now lives in the `commandes` slot, outside
    // this card).
    const w = mountWith({ title: 'first' })
    await w.setProps({ state: full({}) })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.find('[data-player]').exists()).toBe(true)
  })

  it('shows the bar when a position is known', () => {
    const w = mount(PlayerCard, {
      props: {
        state: full({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-bar]').exists()).toBe(true)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('does not show the duration in the header when the bar already carries it', () => {
    // Fixed defect: title + known position (the nominal case of a recognized
    // CD) showed "4:14" in the header AND "1:27 ... 4:14" in the bar, the same
    // information twice.
    const w = mount(PlayerCard, {
      props: {
        state: full({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-duration]').exists()).toBe(false)
    expect(w.get('[data-total-duration]').text()).toBe('4:14')
  })

  it('shows the cover when the device serves one', () => {
    const w = mountWith({ title: 'So What', cover_href: '/api/cover/1a2b', cover_origin: 'files' })
    const img = w.find('[data-cover-image] img')
    expect(img.exists()).toBe(true)
    // The UI must never point outside: the core serves the image. And it asks
    // for the **thumbnail**: the square is 224 px, a NAS's `folder.jpg` is
    // commonly three mebibytes.
    expect(img.attributes('src')).toBe('/api/cover/1a2b?size=thumbnail')
    // The cover's origin is no longer a badge: it is in the provenance
    // popover, with the other fields (see below).
    expect(w.find('[data-cover-origin]').exists()).toBe(false)
  })

  it('details the provenance field by field in a popover', async () => {
    // **What the two origin badges did not say.** They named the contributor
    // of the text and that of the image; the screen is composed by more hands
    // than that — the title from here, the year from there, the cover from
    // elsewhere — and that is the question one asks in front of a wrong title.
    const w = mountWith({
      title: 'So What',
      origin: 'icy',
      cover_href: '/api/cover/1a2b',
      cover_origin: 'musicbrainz',
      provenance: {
        fields: { title: 'icy', year: 'musicbrainz', cover: 'musicbrainz' },
        misses: ['ouifm-metas'],
      },
    })
    // The badges gave way to the button.
    expect(w.find('[data-origin]').exists()).toBe(false)
    expect(w.find('[data-cover-origin]').exists()).toBe(false)

    await w.get('[data-provenance-open]').trigger('click')
    const popover = document.body.querySelector('[data-provenance-popover]')
    expect(popover).not.toBeNull()
    const by = (field: string) =>
      popover?.querySelector(`[data-provenance-field="${field}"]`)?.textContent?.trim()
    expect(by('title')).toBe('icy')
    expect(by('year')).toBe('musicbrainz')
    expect(by('cover')).toBe('musicbrainz')
    // "Searched and found nothing" is a separate section: it is not the same
    // information as an absence from the list above, which also holds when
    // the plugin was never queried.
    expect(popover?.querySelector('[data-provenance-misses]')?.textContent).toContain('ouifm-metas')
    w.unmount()
  })

  it('names the rework next to the source, never in its place', async () => {
    // **The defect reported by the owner**: on a radio without a metadata
    // plugin, ICY gave the information, `musicbrainz` split it, and the screen
    // showed "Title: musicbrainz". The station is the source; the split is
    // said alongside.
    const w = mountWith({
      title: 'Miles Davis - So What',
      provenance: {
        fields: { title: 'icy', artist: 'icy' },
        derived: { title: 'musicbrainz', artist: 'musicbrainz' },
      },
    })
    await w.get('[data-provenance-open]').trigger('click')
    const popover = document.body.querySelector('[data-provenance-popover]')
    const row = popover?.querySelector('[data-provenance-field="title"]')
    // The source, word for word: it is what was being erased.
    expect(row?.textContent).toContain('icy')
    // And the rework exists, **in the same row**. The label itself comes from
    // the catalog, which this mount does not load (`t()` falls back to the
    // key): its wording and the fr/en parity are covered by `i18nKeysUsed`,
    // this test only proves the layout.
    expect(row?.querySelector('[data-provenance-derived="title"]')).not.toBeNull()
    w.unmount()
  })

  it('does not offer the button when there is nothing to explain', () => {
    // A `(?)` that opens an empty popover promises an explanation and gives
    // none: it is the ordinary case before a track is identified.
    const w = mountWith({ title: 'Made Up - TAHITI 80' })
    expect(w.find('[data-provenance-open]').exists()).toBe(false)
  })

  it('keeps the square in place when there is no cover', () => {
    const w = mountWith({ title: 'So What' })
    // The square always exists: the cover arrives after the text, sometimes
    // several seconds after, and a square that appears would shift everything.
    expect(w.find('[data-cover-image]').exists()).toBe(true)
    expect(w.find('[data-cover-image] img').exists()).toBe(false)
    expect(w.find('[data-cover-fallback]').exists()).toBe(true)
  })

  it('falls back to the placeholder when the browser cannot load the cover', async () => {
    // The real case: the core's cache key is capped at a few entries, and the
    // file itself lives on a share that can vanish — both yield a 404 under an
    // already-published URL. Without `@error`, the reserved square showed the
    // browser's broken-image glyph instead of the ♫ fallback intended for
    // exactly this situation.
    const w = mountWith({ title: 'So What', cover_href: '/api/cover/1a2b' })
    await w.get('[data-cover-image] img').trigger('error')
    expect(w.find('[data-cover-image] img').exists()).toBe(false)
    expect(w.find('[data-cover-fallback]').exists()).toBe(true)
    // The square itself does not move: nothing must shift.
    expect(w.find('[data-cover-image]').exists()).toBe(true)

    // And a **different** image gives the element another chance: otherwise a
    // single failure would doom the square for the rest of the session.
    await w.setProps({ state: full({ title: 'So What', cover_href: '/api/cover/3c4d' }) })
    expect(w.get('[data-cover-image] img').attributes('src')).toBe(
      '/api/cover/3c4d?size=thumbnail',
    )
  })

  it('enlarges the cover on click, and closes it on the next click', async () => {
    const w = mountWith({ title: 'So What', cover_href: '/api/cover/1a2b' })
    // Nothing open at the start.
    expect(document.body.querySelector('[data-cover-enlarged]')).toBeNull()

    await w.get('[data-cover-enlarge]').trigger('click')
    const overlay = document.body.querySelector('[data-cover-enlarged]')
    expect(overlay).not.toBeNull()
    // The enlarged view loads the **full** image, not the thumbnail: that is
    // the whole point of enlarging.
    expect(overlay?.querySelector('img')?.getAttribute('src')).toBe('/api/cover/1a2b')

    // The overlay is **teleported to the body**: it does not belong to the
    // wrapper's subtree, so `w.get` does not see it. We drive it through the
    // DOM, as a real click would.
    const close = document.body.querySelector<HTMLElement>('[data-cover-close]')
    expect(close).not.toBeNull()
    close!.click()
    await nextTick()
    expect(document.body.querySelector('[data-cover-enlarged]')).toBeNull()
    w.unmount()
  })

  it('shows a loading indicator while the full-size cover has not loaded yet', async () => {
    // The full size is fetched on demand (Task 8), so opening no longer
    // guarantees the bytes are already there. Fails if a future change goes
    // back to treating the enlarge click as instant and drops the indicator.
    const w = mount(PlayerCard, {
      props: { state: full({ title: 'So What', cover_href: '/api/cover/1a2b' }), seekStep: 10 },
      attachTo: document.body,
    })
    await w.get('[data-cover-enlarge]').trigger('click')
    const overlay = document.body.querySelector('[data-cover-enlarged]')
    expect(overlay).not.toBeNull()
    expect(overlay?.querySelector('[data-cover-enlarged-loading]')).not.toBeNull()
    w.unmount()
  })

  it('hides the loading indicator once the full-size image has loaded, keeping the picture', async () => {
    // Fails if `@load` is not wired to the flag, or wired to something that
    // also tears down the overlay itself (the picture must stay).
    const w = mount(PlayerCard, {
      props: { state: full({ title: 'So What', cover_href: '/api/cover/1a2b' }), seekStep: 10 },
      attachTo: document.body,
    })
    await w.get('[data-cover-enlarge]').trigger('click')
    const overlay = document.body.querySelector('[data-cover-enlarged]')
    const img = overlay?.querySelector('img')
    expect(img).not.toBeNull()
    img!.dispatchEvent(new Event('load'))
    await nextTick()
    expect(document.body.querySelector('[data-cover-enlarged-loading]')).toBeNull()
    expect(document.body.querySelector('[data-cover-enlarged]')).not.toBeNull()
    expect(document.body.querySelector('[data-cover-enlarged] img')).not.toBeNull()
    w.unmount()
  })

  it('keeps the full-size wait independent from the thumbnail\'s own retry flag', async () => {
    // Fails if the two states were folded into one shared flag: the
    // thumbnail's `imageBroken` (with its own retry mechanics, see
    // `onImageError`) would then also toggle or hide the enlarged view's
    // indicator, condemning the player's square to a failure that is not its
    // own -- exactly what the brief forbids.
    const w = mount(PlayerCard, {
      props: { state: full({ title: 'So What', cover_href: '/api/cover/1a2b' }), seekStep: 10 },
      attachTo: document.body,
    })
    await w.get('[data-cover-enlarge]').trigger('click')
    expect(document.body.querySelector('[data-cover-enlarged-loading]')).not.toBeNull()

    // The thumbnail fails and enters its own retry cycle.
    await w.get('[data-cover-image] img').trigger('error')
    expect(document.body.querySelector('[data-cover-enlarged]')).not.toBeNull()
    expect(document.body.querySelector('[data-cover-enlarged-loading]')).not.toBeNull()
    w.unmount()
  })

  it('arms the loading indicator again on a fresh open, even after a previous open finished loading', async () => {
    // Fails if the flag were only ever initialized once (e.g. `ref(true)`
    // read once) instead of being reset by the click handler on every open:
    // a second, genuinely slow fetch (the core's cache evicted the first
    // download) would then show nothing while it works.
    const w = mount(PlayerCard, {
      props: { state: full({ title: 'So What', cover_href: '/api/cover/1a2b' }), seekStep: 10 },
      attachTo: document.body,
    })
    await w.get('[data-cover-enlarge]').trigger('click')
    const firstImg = document.body.querySelector('[data-cover-enlarged] img')
    firstImg!.dispatchEvent(new Event('load'))
    await nextTick()
    expect(document.body.querySelector('[data-cover-enlarged-loading]')).toBeNull()

    await document.body.querySelector<HTMLElement>('[data-cover-close]')!.click()
    await nextTick()
    expect(document.body.querySelector('[data-cover-enlarged]')).toBeNull()

    await w.get('[data-cover-enlarge]').trigger('click')
    expect(document.body.querySelector('[data-cover-enlarged-loading]')).not.toBeNull()
    w.unmount()
  })

  it('closes the enlarged cover when the track changes', async () => {
    // Otherwise the next track's image shows up full screen without anyone
    // asking for it.
    const w = mountWith({ title: 'So What', cover_href: '/api/cover/1a2b' })
    await w.get('[data-cover-enlarge]').trigger('click')
    expect(document.body.querySelector('[data-cover-enlarged]')).not.toBeNull()

    await w.setProps({ state: full({ title: 'Blue in Green', cover_href: '/api/cover/9f9f' }) })
    expect(document.body.querySelector('[data-cover-enlarged]')).toBeNull()
    w.unmount()
  })

  it('does not offer enlarging when there is no cover', () => {
    // A button that opens nothing is worse than no button: the ♫ fallback is
    // not an image.
    const w = mountWith({ title: 'So What' })
    expect(w.find('[data-cover-enlarge]').exists()).toBe(false)
  })

  it('shows nothing of the progress when no position is known', () => {
    const w = mount(PlayerCard, {
      props: {
        state: full({ title: 'Bikwix', position_s: null, duration_s: 254 }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-position]').exists()).toBe(false)
  })

  // Without title, artist or album, the "now playing" block is hidden: the
  // progress, however, must stay visible. This is the case of a file without
  // tags or of a disc MusicBrainz does not recognize, where mpv nonetheless
  // knows the position perfectly well.
  it('shows the progress even without any metadata', () => {
    const w = mount(PlayerCard, {
      props: {
        state: full({ position_s: 87, duration_s: 254, seekable: true }),
        seekStep: 10,
      },
    })
    expect(w.find('[data-now-playing]').exists()).toBe(false)
    expect(w.get('[data-position]').text()).toBe('1:27')
    expect(w.find('[data-bar]').exists()).toBe(true)
  })

  it('the cover and the track are at the center, the source as a pill', () => {
    const w = mountWith({ title: 'Blue in Green', artist: 'Miles Davis', album: 'Kind of Blue', preset: 1, preset_name: 'FIP' })
    expect(w.get('[data-source]').text()).toBe('radio')
    expect(w.get('[data-player-preset]').text()).toBe('1')
    expect(w.get('[data-player-preset-name]').text()).toBe('FIP')
    expect(w.get('[data-title]').classes()).toContain('text-xl')
    expect(w.find('[data-cover-image]').exists()).toBe(true)
  })

  it('the cover square stays even without a track: it is what holds the layout', () => {
    const w = mountWith({ status: 'NO DISC', preset_count: 0 })
    expect(w.find('[data-cover-image]').exists()).toBe(true)
    expect(w.find('[data-cover-fallback]').exists()).toBe(true)
    expect(w.get('[data-player-status]').text()).toBe('NO DISC')
  })

  it('in standby the cover dims', () => {
    const w = mountWith({ standby: true })
    expect(w.get('[data-cover-image]').classes()).toContain('opacity-50')
    expect(w.find('[data-standby]').exists()).toBe(true)
  })

  it('renders the actions and commandes slots', () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const w = mount(PlayerCard, {
      props: { state: full({}), seekStep: 10 },
      slots: { actions: '<button data-test-action>a</button>', commandes: '<div data-test-commands>c</div>' },
    })
    expect(w.find('[data-slot="card-action"] [data-test-action]').exists()).toBe(true)
    expect(w.find('[data-test-commands]').exists()).toBe(true)
  })

  describe('year', () => {
    it('sits next to the album, separated by a middle dot', () => {
      const w = mountWith({ title: 'So What', album: 'Kind of Blue', year: 1959 })
      expect(w.find('[data-album]').text()).toBe('Kind of Blue')
      expect(w.find('[data-year]').text()).toBe('1959')
      // Both on the same line, with the separator between them.
      expect(w.find('[data-album]').element.parentElement?.textContent).toContain('Kind of Blue · 1959')
    })

    it('stands alone when no album is known', () => {
      // Real: a stream may give the year without the album, the Radio France
      // schedule yields one far more often than the other.
      const w = mountWith({ title: 'Fire', album: null, year: 1960 })
      expect(w.find('[data-year]').text()).toBe('1960')
      expect(w.find('[data-album]').exists()).toBe(false)
    })

    it('leaves no trace when it is unknown', () => {
      const w = mountWith({ title: 'So What', album: 'Kind of Blue' })
      expect(w.find('[data-year]').exists()).toBe(false)
    })
  })

  describe('platform links', () => {
    it('renders one icon per platform, as a safe external link', () => {
      const w = mountWith({
        title: 'Get Lucky',
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=5NV6Rdv1a3I' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/9956167' },
        ],
      })
      expect(w.findAll('[data-link]')).toHaveLength(2)
      const yt = w.get('[data-link="youtube"]')
      expect(yt.attributes('href')).toBe('https://www.youtube.com/watch?v=5NV6Rdv1a3I')
      expect(yt.attributes('target')).toBe('_blank')
      // `noopener`: the target is a third party. `noreferrer`: it has no
      // business knowing where we come from.
      expect(yt.attributes('rel')).toBe('noopener noreferrer')
      // A translated accessible name, not a mute icon.
      expect(yt.attributes('aria-label')).toBeTruthy()
      expect(yt.find('svg').exists()).toBe(true)
      expect(w.find('[data-link="deezer"]').exists()).toBe(true)
    })

    it('tells the three platforms apart by their icon', () => {
      const w = mountWith({
        title: 'Get Lucky',
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/1' },
          { platform: 'apple_music', url: 'https://music.apple.com/us/song/1' },
        ],
      })
      // Assertion **per platform** and not "three distinct icons": three
      // different icons may very well be the three wrong ones (two inverted
      // `v-if` branches pass the old version of the test). The brand color
      // belongs to only one of the three icons.
      for (const [platform, color] of Object.entries(ICON_COLOR)) {
        expect(w.get(`[data-link="${platform}"] svg`).html()).toContain(`fill="${color}"`)
      }
      const svg = w.findAll('[data-link] svg').map((s) => s.html())
      expect(new Set(svg).size).toBe(3)
    })

    it('renders the icons on the same row as the origin badges', () => {
      // Owner's decision: a row of their own pushed the volume slider down too
      // far on a phone. The badges row hosts them.
      const w = mountWith({
        title: 'Get Lucky',
        duration_s: 248,
        provenance: { fields: { title: 'musicbrainz' } },
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'deezer', url: 'https://www.deezer.com/track/1' },
          { platform: 'apple_music', url: 'https://music.apple.com/us/song/1' },
        ],
      })
      const row = w.get('[data-badges]').element
      expect(w.get('[data-links]').element.parentElement).toBe(row)
      // The provenance button took the place of the two origin badges.
      expect(w.get('[data-provenance-open]').element.parentElement).toBe(row)
      expect(w.get('[data-duration]').element.parentElement).toBe(row)
    })

    it('gives the anchors a 44 px touch target', () => {
      // 44 px, the recommended minimum finger target: the icon alone (20 px)
      // is missed one time in three from the couch.
      const w = mountWith({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      expect(w.get('[data-link="youtube"]').classes()).toContain('size-11')
      expect(w.get('[data-link="youtube"] svg').classes()).toContain('size-5')
    })

    it('gives the provenance (?) the same 44 px touch target', () => {
      // Same reason as the anchors above, and it shares their line -- but
      // nothing pinned it, and the glyph has since moved into the kit's
      // `HelpButton`, whose `size="touch"` is what carries the 44 px now. A
      // call site that forgot that prop would silently drop to 24 px on the
      // one row where the progress bar thumb is already competing for taps.
      const w = mountWith({
        title: 'Get Lucky',
        provenance: { fields: { title: 'musicbrainz' } },
      })
      expect(w.get('[data-provenance-open]').classes()).toContain('size-11')
      // Named twice: `aria-label` for the screen reader, `title` for the
      // hover. The two System `(?)` only gained the second when the
      // affordance became one component.
      expect(w.get('[data-provenance-open]').attributes('title')).toBeTruthy()
      expect(w.get('[data-provenance-open]').attributes('aria-label')).toBeTruthy()
    })

    it('goes in front of the overflow of the progress bar thumb', () => {
      // The thumb's 44 px hit area overflows 19 px above its track (see
      // ProgressBar.vue), while this row is only 8 px higher: without
      // `relative z-10`, a tap at the bottom of a link anchor would land on
      // the thumb (a SeekTo) rather than on the link. jsdom paints nothing:
      // this test documents the layout, it does not prove it on screen
      // (measured by the controller through Playwright).
      const w = mountWith({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      const classes = w.get('[data-links]').classes()
      expect(classes).toContain('relative')
      expect(classes).toContain('z-10')
    })

    it('reserves the row height even without a link', () => {
      // Without a minimum height, the late arrival of a link (MusicBrainz
      // answers after the title) grew the card and pushed the volume down
      // under the finger already placed.
      const w = mountWith({
        title: 'Get Lucky',
        provenance: { fields: { title: 'icy' } },
      })
      expect(w.get('[data-badges]').classes()).toContain('min-h-11')
    })

    it('does not open the badges row when there is nothing to put in it', () => {
      // A bare title (the most common ICY case) must not reserve 44 empty px
      // under the album.
      const w = mountWith({ title: 'Made Up - TAHITI 80' })
      expect(w.find('[data-badges]').exists()).toBe(false)
    })

    it('opens the badges row for a link alone', () => {
      const w = mountWith({
        title: 'Get Lucky',
        links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }],
      })
      expect(w.find('[data-badges]').exists()).toBe(true)
    })

    it('renders nothing for an unknown platform', () => {
      // The protocol closes the set, but a `v-else` rendered the Apple icon
      // for anything that was neither YouTube nor Deezer: a plugin ahead of
      // the core would have shown "Listen on Apple Music" pointing at Spotify.
      const w = mountWith({
        title: 'Get Lucky',
        links: [{ platform: 'unknown' as 'youtube', url: 'https://example.test/x' }],
      })
      expect(w.findAll('[data-link]')).toHaveLength(0)
    })

    it('renders two anchors for two links from the same platform', () => {
      // Nothing in the protocol forbids two links from the same platform: a
      // render key placed on `platform` would have lost one.
      const w = mountWith({
        title: 'Get Lucky',
        links: [
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' },
          { platform: 'youtube', url: 'https://www.youtube.com/watch?v=b' },
        ],
      })
      expect(w.findAll('[data-link="youtube"]')).toHaveLength(2)
    })

    it('does not show the row when there is no link', () => {
      expect(mountWith({ title: 'So What' }).find('[data-links]').exists()).toBe(false)
      expect(mountWith({ title: 'So What', links: [] }).find('[data-links]').exists()).toBe(false)
    })

    it('stays silent when nothing else is known about the track', () => {
      // The whole area is behind `nothingToShow`: platform icons alone,
      // without title or artist, would be buttons without a subject. Rule
      // inherited from the component, checked here because the links are the
      // first piece of data that could arrive alone.
      const w = mountWith({ links: [{ platform: 'youtube', url: 'https://www.youtube.com/watch?v=a' }] })
      expect(w.find('[data-links]').exists()).toBe(false)
    })
  })
})
