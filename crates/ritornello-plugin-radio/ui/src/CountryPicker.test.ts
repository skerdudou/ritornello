import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import CountryPicker from './CountryPicker.vue'

const LIST = [
  { code: 'DE', stations: 6081 },
  { code: 'FR', stations: 2746 },
  { code: 'BE', stations: 300 },
]

function makeMount(current = '') {
  return mount(CountryPicker, {
    props: {
      list: LIST,
      current,
      allLabel: 'All countries',
      placeholder: 'Country or code',
      emptyLabel: 'No countries',
    },
  })
}

describe('CountryPicker', () => {
  it('lists countries by readable name, not by code', () => {
    // The component uses the browser's language; under jsdom that is
    // `en-US`, so Belgium / France / Germany. The resulting order (BE,
    // FR, DE) differs from the code order (DE, FR, BE) **and** from the
    // input order: that is exactly what we want to verify here. Sorting
    // by an explicit language is covered in `country.test.ts`.
    const w = makeMount()
    const codes = w.findAll('[data-country]').map((b) => b.attributes('data-country'))
    expect(codes[0]).toBe('ALL')
    expect(codes.slice(1)).toEqual(['BE', 'FR', 'DE'])
    expect(w.find('[data-country="FR"]').text()).toContain('2746')
  })

  it('filters as you type', async () => {
    const w = makeMount()
    await w.find('[data-country-filter]').setValue('belg')
    const codes = w.findAll('[data-country]').map((b) => b.attributes('data-country'))
    expect(codes).toEqual(['ALL', 'BE'])
  })

  it('keeps "all countries" reachable regardless of the filter', async () => {
    // This is the way back: filtering it out would be a trap.
    const w = makeMount('FR')
    await w.find('[data-country-filter]').setValue('zzzz')
    expect(w.find('[data-country="ALL"]').exists()).toBe(true)
    expect(w.find('[data-country-empty]').text()).toBe('No countries')
  })

  it('marks the current country', () => {
    const w = makeMount('BE')
    expect(w.find('[data-country="BE"]').attributes('data-active')).toBe('true')
    expect(w.find('[data-country="FR"]').attributes('data-active')).toBe('false')
    expect(w.find('[data-country="ALL"]').attributes('data-active')).toBe('false')
  })

  it('marks "all countries" when no country is chosen', () => {
    expect(makeMount('').find('[data-country="ALL"]').attributes('data-active')).toBe('true')
  })

  it('emits the chosen code, and the emptyLabel string for "all"', async () => {
    const w = makeMount()
    await w.find('[data-country="FR"]').trigger('click')
    await w.find('[data-country="ALL"]').trigger('click')
    expect(w.emitted('choose')).toEqual([['FR'], ['']])
  })

  it('stays usable when the list is empty', () => {
    // Unreachable directory: the picker must not disappear, "all
    // countries" remains selectable.
    const w = mount(CountryPicker, {
      props: { list: [], current: 'FR', allLabel: 'All', placeholder: 'p', emptyLabel: 'No countries' },
    })
    expect(w.find('[data-country="ALL"]').exists()).toBe(true)
    expect(w.find('[data-country-empty]').exists()).toBe(true)
  })
})
