import { UI_CONTRACT } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import MpdAdmin, { contract } from './index'

describe('module contract', () => {
  it('declares the contract version the shell expects', () => {
    // The shell compares this value with its own before mounting the
    // component: a mismatch shows up as "interface to rebuild" rather than
    // breaking the page. Taking it from the kit rather than hard-coding it
    // means a contract bump does not forget this module.
    expect(contract).toBe(UI_CONTRACT)
  })

  it('requires `base`, with no default value', () => {
    // Encoded regression: the name under which a plugin is served comes from
    // `plugins.toml`, hence from the deployment. A module that fell back on
    // "/plugins/mpd/" would be wrong — silently — as soon as an operator
    // declares it under another name, and all its requests would go to a
    // nonexistent plugin.
    const props = (MpdAdmin as unknown as { props: Record<string, { required?: boolean; default?: unknown }> }).props
    expect(props.base!.required).toBe(true)
    expect(props.base!.default).toBeUndefined()
    expect(props.catalog!.required).toBe(true)
  })
})
