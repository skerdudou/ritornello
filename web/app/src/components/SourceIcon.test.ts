import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import SourceIcon from './SourceIcon.vue'
import { SOURCE_ICONS } from './sourceIcons'

describe('SourceIcon', () => {
  it('draws a recognised icon, not the initial', () => {
    const w = mount(SourceIcon, { props: { name: 'cd', icon: 'disc' } })
    expect(w.find('[data-source-icon="disc"]').exists()).toBe(true)
    expect(w.find('[data-source-initial]').exists()).toBe(false)
  })

  it('falls back to the initial for an unknown icon name', () => {
    const w = mount(SourceIcon, { props: { name: 'zed', icon: 'rocket' } })
    expect(w.find('[data-source-initial]').text()).toBe('Z')
    expect(w.find('svg').exists()).toBe(false)
  })

  it('falls back to the initial when no icon was announced', () => {
    for (const icon of [undefined, null]) {
      const w = mount(SourceIcon, { props: { name: 'zed', icon } })
      expect(w.find('[data-source-initial]').text()).toBe('Z')
      expect(w.find('svg').exists()).toBe(false)
    }
  })

  it('draws an svg for every icon of the list', () => {
    for (const icon of SOURCE_ICONS) {
      const w = mount(SourceIcon, { props: { name: 'x', icon } })
      const svg = w.find('svg')
      expect(svg.exists(), icon).toBe(true)
      expect(svg.attributes('data-source-icon')).toBe(icon)
      expect(svg.attributes('aria-hidden')).toBe('true')
    }
  })
})
