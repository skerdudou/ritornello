import { UI_CONTRACT } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import FilesAdmin, { contract } from './index'

describe('module contract', () => {
  it('declares the contract version expected by the shell', () => {
    // The shell compares this value to its own before mounting the
    // component: a mismatch displays as "interface needs rebuilding"
    // rather than breaking the page. Taking it from the kit rather than
    // hardcoding it means a contract bump does not forget this module.
    expect(contract).toBe(UI_CONTRACT)
  })

  it('requires `base`, with no default value', () => {
    // Encoded regression: the name under which a plugin is served comes
    // from `plugins.toml`, hence from the deployment. A module that fell
    // back to "/plugins/files/" would be wrong — silently — as soon as an
    // operator declares it under another name, and all its requests would
    // go to a non-existent plugin.
    const props = (FilesAdmin as unknown as { props: Record<string, { required?: boolean; default?: unknown }> }).props
    expect(props.base!.required).toBe(true)
    expect(props.base!.default).toBeUndefined()
    expect(props.catalog!.required).toBe(true)
  })
})
