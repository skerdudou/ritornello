import { mount } from '@vue/test-utils'
import { ConfigProvider as RekaConfigProvider } from 'reka-ui'
import { describe, expect, it } from 'vitest'
import { h } from 'vue'
import { ConfigProvider } from './components/ui/config-provider'

/** The `scrollBody` reka-ui's own provider ends up with, through the kit's. */
function forwarded(props: Record<string, unknown> | null): unknown {
  return mount({ render: () => h(ConfigProvider, props, () => h('i')) })
    .findComponent(RekaConfigProvider)
    .props('scrollBody')
}

describe('ConfigProvider (kit)', () => {
  // The regression this pins: declared with `defineProps<ConfigProviderProps>()`
  // and no default, the wrapper had Vue cast an absent boolean prop to
  // `false` and hand it on — overriding reka-ui's `true` while claiming to
  // decide nothing. Removing `:scroll-body="false"` from App.vue then changed
  // nothing at all, which made a mutation proof of dropdown-width.spec.ts
  // pass for the wrong reason.
  it('an omitted prop leaves reka-ui its own default', () => {
    const own = mount({ render: () => h(RekaConfigProvider, null, () => h('i')) })
      .findComponent(RekaConfigProvider)
      .props('scrollBody')
    expect(own).toBe(true)
    expect(forwarded(null)).toBe(own)
  })

  it('an explicit value goes through unchanged, false as well as true', () => {
    expect(forwarded({ scrollBody: false })).toBe(false)
    expect(forwarded({ scrollBody: true })).toBe(true)
  })
})
