import { createT, Skeleton, UI_CONTRACT, useSkeleton, type Catalog } from '@ritornello/ui'
import {
  computed,
  defineComponent,
  h,
  ref,
  shallowRef,
  watchEffect,
  type Component,
  type PropType,
  type VNode,
} from 'vue'
import { useCatalog } from '../composables/useCatalog'

export interface PluginModule {
  contract: number
  default: Component
}

// Tracking of the stylesheets already requested, by `${name}?${version}`. A
// `Set` rather than a DOM query by attribute selector: building a CSS
// selector from an arbitrary plugin name (`link[href="..."]`) throws a
// `SyntaxError` outside any `try` if that name contains a quote, which blanks
// the view instead of the explicit error message this file otherwise
// prepares.
//
// Keyed by name **and** version, not by name alone: a plugin relaunched with
// rebuilt assets announces a new fingerprint, and the old key would then
// forever match, leaving the stale sheet injected and the new one never
// requested.
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
 * third-party plugin authors (see "A plugin's UI" in `docs/plugins.md`; the
 * README only points there), which silently depended on the trailing slash.
 * The router besides redirects the slash-less form to the one with a slash
 * (see `router.ts`).
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
const PLUGIN_LAYER = 'plugin'

/** Query suffix carrying an asset fingerprint, empty when there is none. */
function versionQuery(version: string): string {
  return version ? `?v=${encodeURIComponent(version)}` : ''
}

// A plugin's CSS is its own Tailwind pass: we inject it once and leave it in
// place (coming back to the page must not replay a download).
//
// A `<style>@import url(...) layer(plugin)</style>` and not a
// `<link rel="stylesheet">`: it is the only way to file an **external** sheet
// in a named layer, and it holds for a third-party plugin whose CSS we do not
// build. The internal layers of the imported sheet (`theme`, `utilities`)
// become sublayers of `plugin`, so their relative order — the one Tailwind
// computed for that plugin — is preserved.
function ensureStylesheet(name: string, version: string): void {
  const key = `${name}?${version}`
  if (injectedStylesheets.has(key)) return
  injectedStylesheets.add(key)
  const style = document.createElement('style')
  // The plugin name comes from `/api/status`, hence from `plugins.toml`.
  // Quotes and parentheses are the only characters that could escape the
  // `url(...)`: strip them rather than escape them, a plugin name never
  // contains any and a malformed `@import` would be ignored silently.
  //
  // Filtered on the **complete** URL, version query included: it is what
  // protects the `url(...)` regardless of which part of the string a stray
  // character came from.
  const href = `${pluginBase(name)}ui.css${versionQuery(version)}`.replace(/["'()\\\s]/g, '')
  style.setAttribute('data-plugin-sheet', name)
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
    /**
     * Whether the plugin's catalog is still in flight, as `PluginRoute` knows
     * it.
     *
     * Mounting the plugin's component before its catalog has arrived shows the
     * translation **keys** — `col_num`, `btn_save` — which the real labels then
     * replace. Those do not have the same length, so every label of the page
     * shifts a fraction of a second after it appeared.
     *
     * **It also holds the component's mount, not merely its reveal**, and
     * that is the part paid for by a bug reported from use. A curtain of
     * `display: none` hides a component that is fully mounted and running,
     * and some values are computed once, at mount: the kit's `SelectItemText`
     * hands an option's text to its Select in `onMounted` and never re-reads
     * it, so a dropdown built behind the curtain registered the raw key and
     * still showed it when the curtain lifted — `country_fr`, then
     * `arrival_nothing`, both reported. Ordinary bindings recover on the next
     * render; a captured value never does. So the guarantee this prop gives a
     * plugin author is that their component is **never rendered with an
     * unsettled catalog** — see `docs/plugins.md`, "A plugin's UI".
     *
     * The cost, honestly: the plugin's own `onMounted` request no longer
     * overlaps its catalog request. Nil when the catalog wins the race
     * against the JS module, which is the ordinary case (a small JSON that
     * leaves first — the module waits for `statusPending`). When the catalog
     * is the slower of the two, the two requests serialize. And in the one
     * case where the catalog lands after the module *and* after the
     * skeleton's floor has elapsed, the component mounts already revealed and
     * paints its own placeholder, so the reader may see this placeholder, a
     * blank, then the plugin's. Rare, and preferred to a wrong label.
     *
     * Defaults to `false` so that a caller that knows nothing of the catalog —
     * every test that mounts this view directly — behaves as before.
     */
    catalogPending: { type: Boolean, default: false },
    /**
     * Fingerprint of the plugin's UI assets, as `/api/status` relays it.
     *
     * Turns the two stable URLs into ones that never need revalidating. Absent
     * for a plugin that announced none: the plain URL is then used, and the
     * previous behaviour (an `ETag` and a 304 per load) still applies.
     */
    uiVersion: { type: String, default: '' },
    /**
     * Whether `/api/status` has not yet settled once, as `PluginRoute` knows
     * it from `usePlugins().unavailable` and whether it has read a first
     * answer.
     *
     * Holds the module-loading effect back: `<RouterView/>` mounts this view
     * before `/api/status` has answered, so on a direct load `uiVersion` is
     * first `''`, then the real fingerprint a moment later. Letting the
     * effect react to that change would import `ui.js` under two different
     * URLs — evaluating the plugin's module twice, and leaving two
     * `<style data-plugin-sheet="…">` behind, since the first is never
     * removed. Waiting for the final value instead costs one load, at the
     * one moment (a direct load) the versioning exists to speed up.
     *
     * "Settled" means answered *or* failed, never an indefinite wait: an
     * unreachable `/api/status` must still lower this flag, exactly as a
     * plugin that announced no fingerprint — a plugin page withheld forever
     * because `/api/status` is down would be far worse than an uncached
     * asset. Defaults to `false` so a caller unaware of `/api/status` —
     * every test that mounts this view directly — behaves as before.
     */
    statusPending: { type: Boolean, default: false },
    loadModule: {
      type: Function as PropType<(name: string, version: string) => Promise<unknown>>,
      default: (name: string, version: string) =>
        import(/* @vite-ignore */ `/plugins/${name}/ui.js${versionQuery(version)}`),
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

    /**
     * Nothing showable yet: neither the module nor its catalog has settled.
     *
     * An `error` counts as settled — a refusal is a final answer, and holding
     * it back would only delay the one message that tells the user what to do.
     */
    const pending = computed(
      () => props.catalogPending || (!component.value && !error.value),
    )

    // Nothing for the first fraction of a second, a placeholder only if the
    // wait outlasts it. The rhythm lives in the kit so that a plugin UI keeps
    // exactly the same one.
    const skeleton = useSkeleton(pending)

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
      // Held until `/api/status` has settled once — see `statusPending`'s
      // doc. Read synchronously (before the first `await`) so `watchEffect`
      // tracks it and re-runs the instant it drops.
      if (props.statusPending) return
      component.value = null
      error.value = null
      try {
        ensureStylesheet(props.name, props.uiVersion)
        const mod = (await props.loadModule(props.name, props.uiVersion)) as Partial<PluginModule>
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

      /**
       * The placeholder the shell can honestly draw: a generic one.
       *
       * At this point it has not loaded the plugin's module, so it does not
       * know the shape of the page it is about to reveal. A plugin composes a
       * placeholder of its own once mounted and knowing better.
       *
       * `role="status"` carries the only text: the blocks themselves are
       * `aria-hidden`, so a screen reader hears the wait announced once
       * instead of a run of empty boxes. The text is visually hidden — it says
       * nothing the moving blocks do not already say to someone who sees them.
       */
      const placeholder = skeleton.value
        ? h('div', { role: 'status', class: 'space-y-3' }, [
            h('span', { class: 'sr-only' }, t('loading')),
            h(Skeleton, { class: 'h-7 w-48' }),
            h(Skeleton, { class: 'h-4 w-full' }),
            h(Skeleton, { class: 'h-4 w-5/6' }),
            h(Skeleton, { class: 'h-4 w-2/3' }),
          ])
        : null

      let content: VNode | null = null
      // **Not built at all while the catalog is unsettled**, where this used
      // to build it and hide it. `props.catalogPending` alone gates it, never
      // `pending` or `skeleton`: the component must mount the moment its
      // catalog lands, and then wait out the skeleton's floor **hidden**,
      // exactly as before — that is what still lets its own request leave
      // during the tail of the wait. Gating on the reveal instead would push
      // that request behind the whole floor, which is the 300 ms this file
      // was written to avoid.
      if (component.value && !props.catalogPending) {
        // `catalog` must be passed explicitly: `h()` does not forward
        // PluginView's props to the component it mounts (this is not
        // "attribute fallthrough", which only concerns undeclared attributes).
        // Without this relay, every real plugin module — RadioAdmin,
        // InputAdmin — receives `catalog: undefined` and `createT` throws at
        // the first `t(...)` of its template.
        //
        // `base` is part of the **contract** of plugin UIs, just like
        // `catalog`: it is the absolute prefix under which the core serves the
        // plugin's routes. The modules previously built their URLs relatively
        // (`./api/data`), hence resolved against the browser's URL and not
        // against anything the contract guarantees — a silent coupling to the
        // form of the shell's route (see `pluginBase`).
        content = h(component.value, {
          catalog: props.catalog,
          base: pluginBase(props.name),
        })
      } else if (error.value) {
        // The cause is not composed by concatenation but by a `{cause}` of
        // the catalog: word order and punctuation belong to the translator,
        // as everywhere else in this repository.
        const message =
          error.value === 'plugin_unavailable' && props.cause
            ? t('plugin_unavailable_cause', { cause: props.cause })
            : t(error.value)
        content = h('p', { class: 'text-muted-foreground' }, message)
      }

      // Mounted as soon as the module **and** the catalog are there, **shown**
      // only once the wait is over.
      //
      // The curtain keeps a job of its own, and it is not the one it was
      // written for: the floor keeps the placeholder up for a moment after
      // both have landed, and a component only mounted at the end of it would
      // start its own data request that much later — up to 300 ms of pure
      // latency added exactly where it hurts — then paint its own placeholder
      // after a blank gap. Mounted underneath for that tail, it loads during
      // the wait and is usually ready by the time the placeholder gives way.
      //
      // What it no longer does is hide a component built without its
      // catalog: that is now impossible, because a value captured at mount —
      // a Select's option text — never recovered from it (see
      // `catalogPending` above).
      //
      // `display: none` and not a `v-if`: hidden from sight and from assistive
      // technology alike, while the component keeps living. Moving it between
      // two shapes would unmount and remount it — hence fetch twice.
      const revealed = !skeleton.value && !pending.value

      // Always the same two children, in the same order: Vue patches an
      // unkeyed list by position, so a stable shape is what guarantees the
      // component below is never torn down and rebuilt.
      return h('div', [
        placeholder,
        content
          ? h(
              'div',
              {
                'data-plugin-content': '',
                style: revealed ? undefined : { display: 'none' },
              },
              [content],
            )
          : null,
      ])
    }
  },
})
