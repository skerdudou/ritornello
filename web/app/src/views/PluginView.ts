import { createT, UI_CONTRACT, type Catalog } from '@ritornello/ui'
import {
  defineComponent,
  h,
  ref,
  shallowRef,
  watchEffect,
  type Component,
  type PropType,
} from 'vue'
import { useCatalog } from '../composables/useCatalog'

export interface PluginModule {
  contract: number
  default: Component
}

// Tracking of the stylesheets already requested, by plugin name. A `Set`
// rather than a DOM query by attribute selector: building a CSS selector from
// an arbitrary plugin name (`link[href="..."]`) throws a `SyntaxError` outside
// any `try` if that name contains a quote, which blanks the view instead of
// the explicit error message this file otherwise prepares.
const injectedStylesheets = new Set<string>()

/**
 * **Absolute** prefix under which the core serves a plugin's routes, passed to
 * its component through the `base` prop.
 *
 * Without it, `RadioAdmin` and `InputAdmin` called `api.get('./api/data')`
 * relatively — hence resolved against the browser's URL, and not against
 * anything the contract guarantees. Measured consequence: `/plugins/radio/`
 * and `/plugins/radio` both match the Vue router's route (non-strict by
 * default); on the form **without** trailing slash, `./api/data` resolves to
 * `/plugins/api/data`, which the core interprets as the plugin `"api"` -> 404.
 * The page mounted, showed an empty table and a loading error, and all the
 * buttons failed.
 *
 * The URL is cosmetic; the coupling is not: it is a documented contract for
 * third-party plugin authors (see the "Plugin UI" section of the README),
 * which silently depended on the trailing slash. The router besides redirects
 * the slash-less form to the one with a slash (see `router.ts`).
 */
export function pluginBase(name: string): string {
  return `/plugins/${name}/`
}

/**
 * Cascade layer in which every plugin stylesheet is filed.
 *
 * Declared **below** `utilities` by `app.css`, which fixes the order of the
 * layers. This is what prevents a plugin's UI from undoing the shell's layout:
 * both are separate Tailwind passes that emitted into the same layer, and the
 * plugin's, injected later, won at equal specificity. The `.hidden` of the
 * file field of `generic-input` thus made the top menu (`hidden ... md:flex`)
 * disappear for the rest of the session.
 */
const PLUGIN_LAYER = 'greffon'

// A plugin's CSS is its own Tailwind pass: we inject it once and leave it in
// place (coming back to the page must not replay a download).
//
// A `<style>@import url(...) layer(greffon)</style>` and not a
// `<link rel="stylesheet">`: it is the only way to file an **external** sheet
// in a named layer, and it holds for a third-party plugin whose CSS we do not
// build. The internal layers of the imported sheet (`theme`, `utilities`)
// become sublayers of `greffon`, so their relative order — the one Tailwind
// computed for that plugin — is preserved.
function ensureStylesheet(name: string): void {
  if (injectedStylesheets.has(name)) return
  injectedStylesheets.add(name)
  const style = document.createElement('style')
  // The plugin name comes from `/api/status`, hence from `plugins.toml`.
  // Quotes and parentheses are the only characters that could escape the
  // `url(...)`: strip them rather than escape them, a plugin name never
  // contains any and a malformed `@import` would be ignored silently.
  const href = `${pluginBase(name)}ui.css`.replace(/["'()\\\s]/g, '')
  style.setAttribute('data-feuille-greffon', name)
  style.textContent = `@import url("${href}") layer(${PLUGIN_LAYER});`
  document.head.appendChild(style)
}

// Loads a plugin's UI module and mounts it. The plugin name comes from
// `/api/status`: neither this file nor the core knows the list of plugins.
// `loadModule` is only parameterizable for the tests; in production it is a
// dynamic `import()` of `/plugins/<name>/ui.js`.
export default defineComponent({
  name: 'PluginView',
  props: {
    name: { type: String, required: true },
    catalog: { type: Object as PropType<Catalog>, default: () => ({}) },
    /**
     * Cause of the core's refusal, collected by `PluginRoute` on the catalog
     * call — the only one whose body can be read. It is only shown with
     * `plugin_unavailable`: a mismatched contract already says what to do,
     * and the cause of a catalog refusal has nothing to do with it.
     */
    cause: { type: String, default: '' },
    loadModule: {
      type: Function as PropType<(name: string) => Promise<unknown>>,
      default: (name: string) => import(/* @vite-ignore */ `/plugins/${name}/ui.js`),
    },
  },
  setup(props) {
    // `shallowRef`: the loaded component is a complete Vue options object
    // (`defineComponent`, potentially large). A `ref` would make it deeply
    // reactive — useless proxy overhead on every internal property, and the
    // warning `Vue received a Component that was made a reactive object`.
    // `error` stays a `ref`: it is a plain string.
    const component = shallowRef<Component | null>(null)
    // The three loading messages are carried by keys of the **common
    // vocabulary** (`crates/ritornello-i18n/src/locales/common_en.toml` and
    // `deploy/locales/common/fr.toml`), hence inherited by every catalog. They
    // were previously named `unavailable`/`contract`/`loading` and existed in
    // no catalog: `createT` falling back on the key, the user literally read
    // "loading" then "unavailable" or "contract" — the visible failure mode of
    // the whole plugin loading architecture.
    const error = ref<'plugin_unavailable' | 'plugin_contract_mismatch' | null>(null)

    // The shell's catalog, used as fallback. Indispensable: the messages above
    // are resolved in **the plugin's** catalog, which is empty precisely when
    // the plugin is unreachable — the very case that produces
    // `plugin_unavailable`.
    const { t: tShell } = useCatalog()

    // Generation counter: the `watchEffect` is asynchronous and re-runs on
    // every change of `props.name`. Without it, the late resolution of a load
    // A would overwrite the result of a more recent load B if both were one
    // day in flight simultaneously. This is not observable today because
    // `PluginRoute.vue` mounts this component with `:key="name"`, which
    // destroys and recreates it on every name rather than letting
    // `props.name` change in place — but that guarantee lives in another
    // file. The counter makes this file correct by itself, regardless of what
    // the route will do in tasks 9 and 11.
    let generation = 0

    watchEffect(async () => {
      const gen = ++generation
      component.value = null
      error.value = null
      try {
        ensureStylesheet(props.name)
        const mod = (await props.loadModule(props.name)) as Partial<PluginModule>
        if (gen !== generation) return
        if (mod?.contract !== UI_CONTRACT) {
          console.warn(`plugin ${props.name}: contract ${mod?.contract} expected ${UI_CONTRACT}`)
          error.value = 'plugin_contract_mismatch'
          return
        }
        if (!mod.default) {
          console.warn(`plugin ${props.name}: no default component exported`)
          error.value = 'plugin_unavailable'
          return
        }
        component.value = mod.default
      } catch (e) {
        if (gen !== generation) return
        console.warn(`plugin ${props.name}: loading failed`, e)
        error.value = 'plugin_unavailable'
      }
    })

    return () => {
      // An empty plugin catalog means "no catalog": we then take the shell's,
      // which carries the same three keys through the common layer. Read in
      // the render (and not captured once and for all) to stay reactive: the
      // shell's catalog is loaded asynchronously by `App.vue`, hence often
      // after the first render of this view.
      const t = Object.keys(props.catalog).length > 0 ? createT(props.catalog) : tShell.value
      if (error.value) {
        // The cause is not composed by concatenation but by a `{cause}` of
        // the catalog: word order and punctuation belong to the translator,
        // as everywhere else in this repository.
        const message =
          error.value === 'plugin_unavailable' && props.cause
            ? t('plugin_unavailable_cause', { cause: props.cause })
            : t(error.value)
        return h('p', { class: 'text-muted-foreground' }, message)
      }
      if (!component.value) return h('p', { class: 'text-muted-foreground' }, t('loading'))
      // `catalog` must be passed explicitly: `h()` does not forward
      // PluginView's props to the component it mounts (this is not "attribute
      // fallthrough", which only concerns undeclared attributes). Without this
      // relay, every real plugin module — RadioAdmin, InputAdmin — receives
      // `catalog: undefined` and `createT` throws at the first `t(...)` of its
      // template.
      //
      // `base` is part of the **contract** of plugin UIs, just like `catalog`:
      // it is the absolute prefix under which the core serves the plugin's
      // routes. The modules previously built their URLs relatively
      // (`./api/data`), hence resolved against the browser's URL and not
      // against anything the contract guarantees — a silent coupling to the
      // form of the shell's route (see `pluginBase`).
      return h(component.value, { catalog: props.catalog, base: pluginBase(props.name) })
    }
  },
})
