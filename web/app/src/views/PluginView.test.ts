import type { Catalog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { defineComponent, h, type PropType } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import PluginView from './PluginView'

// Les trois cles vivent dans le vocabulaire **commun**
// (`crates/ritornello-i18n/src/locales/common_en.toml`,
// `deploy/locales/common/fr.toml`), donc heritees par tous les catalogues —
// celui du coeur comme celui de chaque plugin.
const CATALOGUE = {
  plugin_unavailable: 'IHM unavailable',
  plugin_unavailable_cause: 'IHM unavailable : {cause}',
  plugin_contract_mismatch: 'Plugin à reconstruire',
  loading: 'Chargement…',
}

function monter(loader: () => Promise<unknown>, name = 'demo', cause = '') {
  return mount(PluginView, { props: { name, loadModule: loader, catalog: CATALOGUE, cause } })
}

describe('PluginView', () => {
  // Le catalogue du shell vit dans un `ref` au niveau du module de
  // `useCatalog` : il persiste entre les `it()`. On le remet a vide avant
  // chaque test pour que seuls les tests qui le peuplent explicitement en
  // beneficient.
  beforeEach(async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 200 })))
    await useCatalog().reload()
    vi.unstubAllGlobals()
  })

  it('mounted le composant du module quand le contract correspond', async () => {
    const vue = defineComponent({ render: () => h('p', 'IHM du plugin') })
    const w = monter(async () => ({ contract: 1, default: vue }))
    await flushPromises()
    expect(w.text()).toContain('IHM du plugin')
  })

  it('transmet le catalogue au composant mounted', async () => {
    // Regression : `h(composant.value)` seul ne fait step suivre les props de
    // PluginView (ce n'est step de l'attribute fallthrough, qui ne concerne
    // que les attributs non declares) — tout module de plugin reel qui
    // declare `catalog` comme prop requise (RadioAdmin, InputAdmin) recevait
    // `undefined` et `createT` levait au premier `t(...)` de son template.
    const vue = defineComponent({
      props: { catalog: { type: Object as PropType<Catalog>, required: true } },
      render(this: { catalog: Catalog }) {
        return h('p', this.catalog.cle ?? 'catalogue absent')
      },
    })
    const w = mount(PluginView, {
      props: {
        name: 'demo-catalogue',
        loadModule: async () => ({ contract: 1, default: vue }),
        catalog: { cle: 'valeur transmise' },
      },
    })
    await flushPromises()
    expect(w.text()).toContain('valeur transmise')
  })

  it('transmet le préfixe absolu `base` au composant monté', async () => {
    // IMPORTANT 6 de la revue finale : `base` fait partie du contract des IHM de
    // plugin, au meme titre que `catalog`. Les modules construisaient leurs URL
    // en relatif (`./api/data`), donc resolues contre l'URL du browser — un
    // couplage silencieux a la forme (slash final ou non) de la route du shell.
    const vue = defineComponent({
      props: { base: { type: String, required: true } },
      render(this: { base: string }) {
        return h('p', this.base)
      },
    })
    const w = mount(PluginView, {
      props: {
        name: 'radio',
        loadModule: async () => ({ contract: 1, default: vue }),
        catalog: CATALOGUE,
      },
    })
    await flushPromises()
    // Prefixe **absolu**, slash final compris : les modules concatenent
    // directement (`${base}api/data`).
    expect(w.text()).toBe('/plugins/radio/')
  })

  it('refuse un contract incompatible avec un message explicite', async () => {
    const avertir = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const vue = defineComponent({ render: () => h('p', 'ne doit step apparaître') })
    const w = monter(async () => ({ contract: 99, default: vue }))
    await flushPromises()
    expect(w.text()).toContain('Plugin à reconstruire')
    expect(w.text()).not.toContain('ne doit step apparaître')
    // Diagnostic propre a cette branche : distingue ce cas de l'echec de
    // chargement et de l'absence d'export par defaut, qui partagent le meme
    // message displayed a l'ecran ('IHM unavailable' n'est step concerne ici,
    // mais le principe vaut pour les deux tests suivants).
    expect(avertir).toHaveBeenCalledWith(expect.stringContaining('contract 99 expected 1'))
  })

  it('displayed l’indisponibilité quand le module ne load step', async () => {
    const avertir = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = monter(async () => {
      throw new Error('404')
    })
    await flushPromises()
    expect(w.text()).toContain('IHM unavailable')
    // Le message displayed est identique a celui du test suivant : c'est le
    // `console.warn` qui distingue reellement les deux branches.
    expect(avertir).toHaveBeenCalledWith(
      expect.stringContaining('chargement impossible'),
      expect.anything(),
    )
  })

  it('displayed l’indisponibilité quand le module n’exporte step de composant', async () => {
    const avertir = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = monter(async () => ({ contract: 1 }))
    await flushPromises()
    expect(w.text()).toContain('IHM unavailable')
    expect(avertir).toHaveBeenCalledWith(expect.stringContaining('aucun composant par defaut'))
  })

  it('nomme la cause du refus quand la route l’a recueillie', async () => {
    // Le module est load par `import()`, dont l'echec ne livre aucun corps
    // exploitable : la cause ne peut venir que de l'appel a `api/i18n`, un
    // `fetch` dont le corps se lit. `PluginRoute` la recueille et la passe
    // ici. Sans elle, l'ecran disait « IHM unavailable » et rien de plus, au
    // moment ou l'on a le plus besoin de savoir pourquoi.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = monter(
      async () => {
        throw new Error('404')
      },
      'demo',
      'le plugin a mis plus de 5 s a repondre',
    )
    await flushPromises()
    expect(w.text()).toBe('IHM unavailable : le plugin a mis plus de 5 s a repondre')
  })

  it('sans cause connue, le message generique reste tel quel', async () => {
    // Le module peut fail alors que le plugin repond tres bien : un `dist`
    // absent, un contract qui ne correspond step. Inventer une cause serait
    // pire que de n'en donner aucune.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = monter(async () => {
      throw new Error('404')
    })
    await flushPromises()
    expect(w.text()).toBe('IHM unavailable')
  })

  it('une cause ne s’ajoute step à un contract qui ne correspond step', async () => {
    // Ce message-la dit deja quoi faire (reconstruire l'IHM du plugin), et la
    // cause d'un refus de catalogue n'a rien a voir avec lui.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const vue = defineComponent({ render: () => h('p', 'ihm') })
    const w = monter(async () => ({ contract: 99, default: vue }), 'demo', 'peu importe')
    await flushPromises()
    expect(w.text()).toBe('Plugin à reconstruire')
  })

  // --- IMPORTANT 4 de la revue finale : les trois messages etaient affiches
  // en cles brutes ---
  //
  // `t('unavailable')`, `t('contract')` et `t('loading')` ne correspondaient a
  // AUCUNE cle d'aucun catalogue (ni `common_en.toml`, ni l'anglais du coeur,
  // ni les packs de `deploy/locales/`). `createT` retombant sur la cle,
  // l'utilisateur lisait litteralement « loading » puis « unavailable » ou
  // « contract » — et « contract » ne disait rien du « message traduit indiquant
  // que le plugin doit etre reconstruit » exige par la spec.
  it('aucun des trois messages n’est affiché en clé brute', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const cles = ['loading', 'plugin_unavailable', 'plugin_contract_mismatch'] as const
    // Les trois etats qui produisent chacun des trois messages.
    const vue = defineComponent({ render: () => h('p', 'ihm') })
    const jamaisResolu = monter(() => new Promise<never>(() => {})) // reste en chargement
    const contratKo = monter(async () => ({ contract: 99, default: vue }), 'contract-ko')
    const injoignable = monter(async () => {
      throw new Error('404')
    }, 'injoignable')
    await flushPromises()

    const textes = [jamaisResolu.text(), contratKo.text(), injoignable.text()]
    expect(textes).toEqual(['Chargement…', 'Plugin à reconstruire', 'IHM unavailable'])
    // Le vrai invariant : aucun text displayed ne doit etre egal a sa cle.
    for (const text of textes) {
      expect(cles).not.toContain(text)
    }
  })

  it('plugin injoignable (catalogue vide) : le message vient du catalogue du shell', async () => {
    // Le pire cas du diagnostic d'origine : ces cles etaient resolues dans le
    // catalogue **du plugin**, vide precisement quand le plugin est
    // injoignable — le cas qui produit `plugin_unavailable`. Le repli sur le
    // catalogue du shell (`useCatalog`) est donc ce qui rend le message
    // lisible dans le seul cas ou il count vraiment.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ plugin_unavailable: 'Plugin injoignable' }), { status: 200 })),
    )
    await useCatalog().reload() // le shell a son catalogue (load par App.vue)

    // `catalog: {}` : c'est exactement ce que `PluginRoute.vue` transmet quand
    // `GET /plugins/<nom>/api/i18n` echoue.
    const w = mount(PluginView, {
      props: {
        name: 'catalogue-vide',
        loadModule: async () => {
          throw new Error('404')
        },
        catalog: {},
      },
    })
    await flushPromises()
    expect(w.text()).toBe('Plugin injoignable')
  })

  it('injecte la feuille de style du plugin une seule fois', async () => {
    document.head.innerHTML = ''
    const vue = defineComponent({ render: () => h('p', 'ok') })
    // Nom dedie a ce test (plutot que le 'demo' partage par les autres) :
    // le suivi des feuilles injectees vit dans un `Set` au niveau du module
    // de `PluginView.ts`, qui persiste entre les `it()` de ce fichier —
    // reutiliser 'demo' ferait dependre le resultat de l'order d'execution.
    monter(async () => ({ contract: 1, default: vue }), 'feuille-unique')
    monter(async () => ({ contract: 1, default: vue }), 'feuille-unique')
    await flushPromises()
    expect(document.head.querySelectorAll('style[data-feuille-greffon="feuille-unique"]')).toHaveLength(1)
  })

  it('range la feuille du plugin dans la couche `greffon`', async () => {
    // **La regression target est le menu du haut qui disparait.** Les deux
    // passes Tailwind (shell et plugin) ecrivaient dans la meme couche
    // `utilities` ; la feuille du plugin, injectee apres, gagnait a
    // specificite egale, et son `.hidden` (le champ de fichier d'InputAdmin)
    // ecrasait le `md:flex` de `data-nav-haut`. Sans cette assertion, revenir
    // a un `<link rel="stylesheet">` ne casserait aucun autre test.
    document.head.innerHTML = ''
    const vue = defineComponent({ render: () => h('p', 'ok') })
    monter(async () => ({ contract: 1, default: vue }), 'feuille-en-couche')
    await flushPromises()
    const style = document.head.querySelector('style[data-feuille-greffon="feuille-en-couche"]')
    expect(style?.textContent).toBe(
      '@import url("/plugins/feuille-en-couche/ui.css") layer(greffon);',
    )
  })
})
