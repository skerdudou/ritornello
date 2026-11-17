import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { initTheme } from '../composables/useTheme'
import { MoonIcon, SunIcon } from '@radix-icons/vue'
import ThemeToggle from './ThemeToggle.vue'

// Radix icons drop `data-*` attributes, and a marker computed beside the
// `v-if` would only repeat its condition: the icon is told by its own path.
const pathOf = (c: typeof SunIcon) => mount(c).find('path').attributes('d')
const SUN = pathOf(SunIcon)
const MOON = pathOf(MoonIcon)

function icon(w: ReturnType<typeof mount>): 'sun' | 'moon' | 'other' {
  const d = w.find('[data-theme-mode] path').attributes('d')
  return d === SUN ? 'sun' : d === MOON ? 'moon' : 'other'
}

function modeKey(w: ReturnType<typeof mount>) {
  return w.find('[data-theme-mode]')
}

describe('ThemeToggle', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 204 })))
    window.__RITORNELLO_THEME__ = { theme: 'northern-lights', mode: 'light' }
    initTheme()
  })

  it('shows the state, not the next action: the sun in light mode, no A', () => {
    const w = mount(ThemeToggle)
    expect(modeKey(w).attributes('data-theme-mode')).toBe('light')
    expect(modeKey(w).attributes('title')).toBe('theme_mode_light')
    expect(w.find('[data-theme-mode-auto]').exists()).toBe(false)
    w.unmount()
  })

  it('cycles through the three modes, the A appearing only on auto', async () => {
    const w = mount(ThemeToggle)
    await modeKey(w).trigger('click')
    expect(modeKey(w).attributes('data-theme-mode')).toBe('dark')
    expect(modeKey(w).attributes('title')).toBe('theme_mode_dark')
    expect(w.find('[data-theme-mode-auto]').exists()).toBe(false)

    await modeKey(w).trigger('click')
    expect(modeKey(w).attributes('data-theme-mode')).toBe('auto')
    expect(modeKey(w).attributes('title')).toBe('theme_mode_auto')
    // The A is hidden from assistive tech, so the name carries the state.
    expect(modeKey(w).attributes('aria-label')).toBe('theme_mode_toggle (theme_mode_auto)')
    // The A sits on the icon of the mode actually painted; it is decoration,
    // the state is said by `title`.
    const a = w.find('[data-theme-mode-auto]')
    expect(a.text()).toBe('A')
    expect(a.attributes('aria-hidden')).toBe('true')

    await modeKey(w).trigger('click')
    expect(modeKey(w).attributes('data-theme-mode')).toBe('light')
    expect(w.find('[data-theme-mode-auto]').exists()).toBe(false)
    w.unmount()
  })

  it('the icon is the mode painted: sun in light, moon in dark', () => {
    // The two-state key showed the opposite (the sun in dark mode, as the
    // action "go light"); with three states only the current one makes sense.
    const light = mount(ThemeToggle)
    expect(icon(light)).toBe('sun')
    light.unmount()

    window.__RITORNELLO_THEME__ = { theme: 'northern-lights', mode: 'dark' }
    initTheme()
    const dark = mount(ThemeToggle)
    expect(icon(dark)).toBe('moon')
    dark.unmount()
  })

  it('on auto, the icon is the one of the mode the system resolves to', () => {
    const system = (dark: boolean) =>
      ({
        get __RITORNELLO_THEME__() {
          return { theme: 'northern-lights', mode: 'auto' }
        },
        matchMedia: () => ({ matches: dark, addEventListener() {}, removeEventListener() {} }),
      }) as unknown as Window

    initTheme(system(true))
    const night = mount(ThemeToggle)
    expect(icon(night)).toBe('moon')
    expect(night.find('[data-theme-mode-auto]').exists()).toBe(true)
    night.unmount()

    initTheme(system(false))
    const day = mount(ThemeToggle)
    expect(icon(day)).toBe('sun')
    expect(day.find('[data-theme-mode-auto]').exists()).toBe(true)
    day.unmount()
  })
})
