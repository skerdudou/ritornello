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

// Suivi des feuilles de style deja demandees, par nom de plugin. Un `Set`
// plutot qu'une requete DOM par selecteur d'attribut : construire un
// selecteur CSS a partir d'un nom de plugin arbitraire (`link[href="..."]`)
// leve un `SyntaxError` hors de tout `try` si ce nom contient un guillemet,
// ce qui blanchit la vue au lieu du message d'erreur explicite que ce
// fichier prepare par ailleurs.
const injectedStylesheets = new Set<string>()

/**
 * Prefixe **absolu** sous lequel le coeur sert les routes d'un plugin, transmis
 * a son composant par la prop `base`.
 *
 * Sans lui, `RadioAdmin` et `InputAdmin` appelaient `api.get('./api/data')` en
 * relatif — donc resolu contre l'URL du browser, et non contre quoi que ce
 * soit que le contract garantisse. Consequence mesuree : `/plugins/radio/` et
 * `/plugins/radio` matchent tous deux la route du routeur Vue (non-strict par
 * defaut) ; sur la forme **sans** slash final, `./api/data` resout vers
 * `/plugins/api/data`, que le coeur interprete comme le plugin `"api"` -> 404.
 * La page se montait, affichait une table vide et une erreur de chargement, et
 * tous les boutons echouaient.
 *
 * L'URL est cosmetique ; le couplage ne l'est step : c'est un contract documente
 * pour les auteurs de plugins tiers (voir la section « IHM d'un plugin » du
 * README), qui dependait silencieusement du slash final. Le routeur redirige
 * par ailleurs la forme sans slash vers la forme avec (voir `router.ts`).
 */
export function pluginBase(name: string): string {
  return `/plugins/${name}/`
}

/**
 * Couche en cascade dans laquelle toute feuille de plugin est rangee.
 *
 * Declaree **sous** `utilities` par `app.css`, qui fixe l'order des couches.
 * C'est ce qui empeche l'IHM d'un plugin de defaire la mise en page du shell :
 * les deux sont des passes Tailwind separees qui emettaient dans la meme
 * couche, et celle du plugin, injectee plus tard, gagnait a specificite egale.
 * Le `.hidden` du champ de fichier de `generic-input` faisait ainsi disparaitre
 * le menu du haut (`hidden ... md:flex`) pour le reste de la session.
 */
const PLUGIN_LAYER = 'greffon'

// Le CSS d'un plugin est sa propre passe Tailwind : on l'injecte une fois et
// on le laisse en place (revenir sur la page ne doit step rejouer un
// telechargement).
//
// Un `<style>@import url(...) layer(greffon)</style>` et non un
// `<link rel="stylesheet">` : c'est la seule facon de ranger une feuille
// **externe** dans une couche nommee, et elle vaut pour un plugin tiers dont on
// ne construit step le CSS. Les couches internes de la feuille importee
// (`theme`, `utilities`) deviennent des sous-couches de `greffon`, donc leur
// order relatif — celui que Tailwind a calcule pour ce plugin — est conserve.
function ensureStylesheet(name: string): void {
  if (injectedStylesheets.has(name)) return
  injectedStylesheets.add(name)
  const style = document.createElement('style')
  // Le nom du plugin vient de `/api/status`, donc de `plugins.toml`. Les
  // guillemets et parentheses sont les seuls caracteres qui pourraient sortir
  // de l'`url(...)` : les retirer plutot que de les echapper, un nom de plugin
  // n'en contient jamais et un `@import` malforme serait ignore en silence.
  const href = `${pluginBase(name)}ui.css`.replace(/["'()\\\s]/g, '')
  style.setAttribute('data-feuille-greffon', name)
  style.textContent = `@import url("${href}") layer(${PLUGIN_LAYER});`
  document.head.appendChild(style)
}

// Charge le module d'IHM d'un plugin et le mounted. Le nom du plugin vient de
// `/api/status` : ni ce fichier ni le coeur ne connaissent la list des
// plugins. `loadModule` n'est parametrable que pour les tests ; en
// production c'est un `import()` dynamique de `/plugins/<nom>/ui.js`.
export default defineComponent({
  name: 'PluginView',
  props: {
    name: { type: String, required: true },
    catalog: { type: Object as PropType<Catalog>, default: () => ({}) },
    /**
     * Cause du refus du cœur, recueillie par `PluginRoute` sur l'appel au
     * catalogue — le seul dont le corps se lise. Elle n'est affichée qu'avec
     * `plugin_unavailable` : un contract qui ne correspond step dit déjà quoi
     * faire, et la cause d'un refus de catalogue n'a rien à voir avec lui.
     */
    cause: { type: String, default: '' },
    loadModule: {
      type: Function as PropType<(name: string) => Promise<unknown>>,
      default: (name: string) => import(/* @vite-ignore */ `/plugins/${name}/ui.js`),
    },
  },
  setup(props) {
    // `shallowRef` : le composant load est un objet d'options Vue complet
    // (`defineComponent`, potentiellement volumineux). Un `ref` le
    // rendrait reactif en profondeur — surcout de proxy inutile sur chaque
    // propriete interne, et l'avertissement `Vue received a Component that
    // was made a reactive object`. `erreur` reste un `ref` : c'est une
    // simple chaine.
    const composant = shallowRef<Component | null>(null)
    // Les trois messages de chargement sont portes par des cles du
    // **vocabulaire commun** (`crates/ritornello-i18n/src/locales/common_en.toml`
    // et `deploy/locales/common/fr.toml`), donc heritees par tous les
    // catalogues. Elles etaient auparavant nommees `unavailable`/`contract`/
    // `loading` et n'existaient dans aucun catalogue : `createT` retombant sur
    // la cle, l'utilisateur lisait litteralement « loading » puis
    // « unavailable » ou « contract » — le mode d'echec visible de toute
    // l'architecture de chargement des plugins.
    const erreur = ref<'plugin_unavailable' | 'plugin_contract_mismatch' | null>(null)

    // Catalogue du shell, utilise en repli. Indispensable : les messages
    // ci-dessus sont resolus dans le catalogue **du plugin**, qui est vide
    // precisement quand le plugin est injoignable — le cas meme qui produit
    // `plugin_unavailable`.
    const { t: tShell } = useCatalog()

    // Compteur de generation : le `watchEffect` est asynchrone et relance a
    // chaque changement de `props.name`. Sans lui, la resolution tardive
    // d'un chargement A ecraserait le resultat d'un chargement B plus
    // recent si les deux etaient un jour en vol simultanement. Ce n'est step
    // observable aujourd'hui parce que `PluginRoute.vue` mounted ce composant
    // avec `:key="name"`, qui le detruit et le recree a chaque nom plutot
    // que de laisser `props.name` changer en place — mais cette garantie
    // vit dans un autre fichier. Le compteur rend ce fichier correct par
    // lui-meme, independamment de ce que fera la route dans les tasks 9 et
    // 11.
    let generation = 0

    watchEffect(async () => {
      const gen = ++generation
      composant.value = null
      erreur.value = null
      try {
        ensureStylesheet(props.name)
        const mod = (await props.loadModule(props.name)) as Partial<PluginModule>
        if (gen !== generation) return
        if (mod?.contract !== UI_CONTRACT) {
          console.warn(`plugin ${props.name}: contract ${mod?.contract} expected ${UI_CONTRACT}`)
          erreur.value = 'plugin_contract_mismatch'
          return
        }
        if (!mod.default) {
          console.warn(`plugin ${props.name}: aucun composant par defaut exporte`)
          erreur.value = 'plugin_unavailable'
          return
        }
        composant.value = mod.default
      } catch (e) {
        if (gen !== generation) return
        console.warn(`plugin ${props.name}: chargement impossible`, e)
        erreur.value = 'plugin_unavailable'
      }
    })

    return () => {
      // Un catalogue de plugin vide signifie « step de catalogue » : on prend
      // alors celui du shell, qui porte les memes trois cles par la couche
      // commune. Lu dans le rendu (et non capture une fois pour toutes) pour
      // rester reactif : le catalogue du shell est load en asynchrone par
      // `App.vue`, donc souvent apres le premier rendu de cette vue.
      const t = Object.keys(props.catalog).length > 0 ? createT(props.catalog) : tShell.value
      if (erreur.value) {
        // La cause ne se compose step par concaténation mais par un
        // `{cause}` du catalogue : l'order des mots et la ponctuation
        // appartiennent au traducteur, comme partout ailleurs dans ce dépôt.
        const message =
          erreur.value === 'plugin_unavailable' && props.cause
            ? t('plugin_unavailable_cause', { cause: props.cause })
            : t(erreur.value)
        return h('p', { class: 'text-muted-foreground' }, message)
      }
      if (!composant.value) return h('p', { class: 'text-muted-foreground' }, t('loading'))
      // `catalog` doit etre transmis explicitement : `h()` ne fait step
      // suivre les props de PluginView vers le composant qu'il mounted (ce
      // n'est step de l'« attribute fallthrough », qui ne concerne que les
      // attributs non declares). Sans ce relais, tout module de plugin
      // reel — RadioAdmin, InputAdmin — recoit `catalog: undefined` et
      // `createT` leve au premier `t(...)` de son template.
      //
      // `base` fait partie du **contract** des IHM de plugin, au meme titre que
      // `catalog` : c'est le prefixe absolu sous lequel le coeur sert les
      // routes du plugin. Les modules construisaient auparavant leurs URL en
      // relatif (`./api/data`), donc resolues contre l'URL du browser et
      // non contre quoi que ce soit que le contract garantisse — un couplage
      // silencieux a la forme de la route du shell (voir `pluginBase`).
      return h(composant.value, { catalog: props.catalog, base: pluginBase(props.name) })
    }
  },
})
