import { describe, expect, it } from 'vitest'
import { createT } from './i18n'

describe('createT', () => {
  it('resolves a present key', () => {
    const t = createT({ saved: 'Saved' })
    expect(t('saved')).toBe('Saved')
  })

  it('falls back to the key itself when it is missing', () => {
    const t = createT({})
    expect(t('unknown')).toBe('unknown')
  })

  it('interpolates named tokens the way the Rust does', () => {
    const t = createT({ bad_request: 'Invalid request: {detail}' })
    expect(t('bad_request', { detail: 'duplicate preset' })).toBe(
      'Invalid request: duplicate preset',
    )
  })

  it('interpolates a numeric token and leaves unprovided tokens intact', () => {
    const t = createT({ msg: '{n} of {total}' })
    expect(t('msg', { n: 3 })).toBe('3 of {total}')
  })

  it('does not interpret the value: a straight apostrophe passes through as is', () => {
    // This is precisely what the old `{{key}}` substitution broke (Critical
    // defect of dbfa771): here the value is data, never source, so no
    // character is dangerous.
    const t = createT({ hint: "you haven't picked a device yet" })
    expect(t('hint')).toBe("you haven't picked a device yet")
  })
})
