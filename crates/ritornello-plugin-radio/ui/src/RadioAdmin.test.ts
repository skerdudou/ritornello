import { Dialog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import PaysPicker from './PaysPicker.vue'
import RadioAdmin from './RadioAdmin.vue'

const CATALOGUE = {
  btn_add: 'Ajouter', btn_save: 'Enregistrer', btn_search: 'Chercher',
  btn_add_result: '+', saved: 'Enregistré', save_error: 'Échec : ',
  limit_reached: '99 maximum', empty_query: 'Saisir un terme',
  searching: 'Recherche…', no_results: 'Aucun résultat',
  col_num: 'N°', col_name: 'Nom', col_url: 'URL',
  search_title: 'Annuaire', search_placeholder: 'nom', country_label: 'Pays',
  country_all: 'Tous', country_filter_placeholder: 'Pays ou code',
  country_none: 'Aucun pays', country_loading: 'Chargement…',
  reorder_hint: 'Glisser', move_up: 'Monter', move_down: 'Descendre',
  load_error_1: 'Erreur : ', load_error_2: '',
}

// Prefixe absolu que le shell passe par la prop `base` (requise) : c'est le
// contrat, cette vue ne connait pas le nom sous lequel elle est servie.
const BASE = '/plugins/radio/'

function reponses(data: unknown) {
  return vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') return new Response(null, { status: 204 })
    return new Response(JSON.stringify(data), { status: 200 })
  })
}

async function monter(data: unknown = { stations: [], search: [] }) {
  const spy = reponses(data)
  vi.stubGlobal('fetch', spy)
  const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, spy }
}

describe('RadioAdmin', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('charge les stations triées par présélection', async () => {
    const { w } = await monter({
      stations: [
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 1, name: 'A', url: 'http://a' },
      ],
      search: [],
    })
    const noms = w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    expect(noms).toEqual(['A', 'B'])
  })

  it('numérote par position et renumérote après suppression', async () => {
    const { w } = await monter({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 3, name: 'C', url: 'http://c' },
      ],
      search: [],
    })
    await w.findAll('[data-station-delete]')[0]!.trigger('click')
    expect(w.findAll('[data-station-num]').map((n) => n.text())).toEqual(['1', '2'])
    expect(w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value))
      .toEqual(['B', 'C'])
  })

  it('accepte une dixième station : la borne suit désormais le serveur (1..=99)', async () => {
    const stations = Array.from({ length: 9 }, (_, i) => ({
      preset: i + 1, name: `S${i}`, url: `http://s${i}`,
    }))
    const { w } = await monter({ stations, search: [] })
    await w.find('[data-add]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(10)
  })

  it('refuse une centième station avec un message', async () => {
    const stations = Array.from({ length: 99 }, (_, i) => ({
      preset: i + 1, name: `S${i}`, url: `http://s${i}`,
    }))
    const { w } = await monter({ stations, search: [] })
    await w.find('[data-add]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(99)
    expect(w.text()).toContain('99 maximum')
  })

  it('envoie la présélection déduite de la position à l’enregistrement', async () => {
    const { w, spy } = await monter({
      stations: [{ preset: 1, name: 'A', url: 'http://a' }],
      search: [],
    })
    await w.find('[data-add]').trigger('click')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const appel = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((appel![1] as RequestInit).body))).toEqual({
      op: 'save',
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: '', url: '' },
      ],
    })
  })

  it('recherche dans l’annuaire puis relit les résultats', async () => {
    const { w, spy } = await monter({
      stations: [],
      country: 'FR',
      search: [{ name: 'FIP', url: 'http://fip', codec: 'MP3', bitrate: 128, country: 'FR' }],
    })
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((put![1] as RequestInit).body))).toEqual({
      op: 'search', query: 'fip', country: 'FR',
    })
    expect(w.text()).toContain('FIP')
    expect(w.text()).toContain('128')
  })

  it('reprend le pays mémorisé par le plugin et l’affiche traduit', async () => {
    // Defaut corrige : le libelle venait du composant `Select`, qui capture le
    // texte de l'element selectionne au premier rendu — or `PluginView` monte
    // l'IHM avec un catalogue **vide**, donc la page affichait la cle de
    // traduction elle-meme (« country_fr »). Le libelle est desormais rendu
    // depuis le code, par `Intl.DisplayNames`.
    const { w } = await monter({ stations: [], search: [], country: 'DE' })
    expect(w.find('[data-country-open]').text()).toBe('Germany')
  })

  it('affiche « tous les pays » quand aucun pays n’est mémorisé', async () => {
    // Chaine vide = choix legitime, et non absence de valeur : c'est ce que le
    // plugin attend dans `country`.
    const { w, spy } = await monter({ stations: [], search: [], country: '' })
    expect(w.find('[data-country-open]').text()).toBe('Tous')
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((put![1] as RequestInit).body))).toEqual({
      op: 'search', query: 'fip', country: '',
    })
  })

  it('ne demande la liste des pays qu’à l’ouverture du sélecteur, et une seule fois', async () => {
    // Simulacre fidèle au plugin : `get_data` ne rend la liste **qu'après**
    // l'opération `countries`. Un simulacre qui la rendrait dès le montage
    // masquerait la récupération ; un simulacre qui la rendrait toujours vide
    // ferait croire à une redemande à chaque ouverture.
    let recuperee = false
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        if (JSON.parse(String(init.body)).op === 'countries') recuperee = true
        return new Response(null, { status: 204 })
      }
      const corps = {
        stations: [],
        search: [],
        countries: recuperee ? [{ code: 'BE', stations: 300 }] : [],
      }
      return new Response(JSON.stringify(corps), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()

    const puts = () =>
      spy.mock.calls
        .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
        .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    // Au chargement de la page, aucun appel : rien ne le justifie tant que
    // l'utilisateur ne cherche pas à changer de pays.
    expect(puts()).toEqual([])

    // `update:open` plutôt qu'un vrai clic : le contenu d'un Dialog reka n'est
    // monté que lorsqu'il est ouvert, et c'est l'état qui déclenche la
    // récupération, pas le geste.
    await w.findComponent(Dialog).vm.$emit('update:open', true)
    await flushPromises()
    expect(puts()).toEqual(['countries'])

    // Refermer puis rouvrir ne redemande rien : la liste est mémorisée.
    await w.findComponent(Dialog).vm.$emit('update:open', false)
    await w.findComponent(Dialog).vm.$emit('update:open', true)
    await flushPromises()
    expect(puts()).toEqual(['countries'])
  })

  it('le pays choisi dans le sélecteur part dans la recherche', async () => {
    const { w, spy } = await monter({
      stations: [],
      search: [],
      country: '',
      countries: [{ code: 'BE', stations: 300 }],
    })
    await w.findComponent(Dialog).vm.$emit('update:open', true)
    await flushPromises()
    await w.findComponent(PaysPicker).vm.$emit('choose', 'BE')
    await flushPromises()
    expect(w.find('[data-country-open]').text()).toBe('Belgium')

    await w.find('[data-query]').setValue('rock')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)))
      .find((b) => b.op === 'search')
    expect(put).toEqual({ op: 'search', query: 'rock', country: 'BE' })
  })

  it('glisser une station la déplace, et la présélection suit la position', async () => {
    const { w, spy } = await monter({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 3, name: 'C', url: 'http://c' },
      ],
      search: [],
    })
    const lignes = () => w.findAll('[data-station-row]')
    await lignes()[0]!.trigger('dragstart')
    await lignes()[2]!.trigger('drop')
    const noms = w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    expect(noms).toEqual(['B', 'C', 'A'])

    // La présélection **est** la position : c'est ce que l'enregistrement envoie.
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)))
      .find((b) => b.op === 'save')
    expect(put.stations).toEqual([
      { preset: 1, name: 'B', url: 'http://b' },
      { preset: 2, name: 'C', url: 'http://c' },
      { preset: 3, name: 'A', url: 'http://a' },
    ])
  })

  it('les boutons monter/descendre déplacent aussi, et sont bornés', async () => {
    // Le glisser-déposer n'est ni au clavier ni fiable au doigt : ces boutons
    // sont le chemin accessible, pas un ornement.
    const { w } = await monter({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
      ],
      search: [],
    })
    const noms = () =>
      w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    await w.findAll('[data-station-down]')[0]!.trigger('click')
    expect(noms()).toEqual(['B', 'A'])
    await w.findAll('[data-station-up]')[1]!.trigger('click')
    expect(noms()).toEqual(['A', 'B'])
    // Aux extrémités, les boutons sont désactivés.
    expect(w.findAll('[data-station-up]')[0]!.attributes('disabled')).toBeDefined()
    expect(w.findAll('[data-station-down]')[1]!.attributes('disabled')).toBeDefined()
  })

  it('une requête vide n’émet rien et affiche le message dédié', async () => {
    const { w, spy } = await monter()
    await w.find('[data-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Saisir un terme')
  })

  it('vol unique : un second déclenchement pendant une recherche n’émet rien', async () => {
    // Le SDK sert les requetes d'admin strictement en serie : une seconde
    // recherche mise en file derriere la premiere depasserait le plafond de
    // 5 s du coeur, qui repondrait par la phrase traduite de son catalogue
    // (`plugin_timeout`) plutot qu'un code nu.
    let debloquer: () => void = () => {}
    const enCours = new Promise<void>((r) => (debloquer = r))
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        await enCours
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ stations: [], search: [] }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await w.find('[data-search]').trigger('click')
    await w.find('[data-query]').trigger('keydown', { key: 'Enter' })
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(1)
    debloquer()
    await flushPromises()
    // L'etat est rétabli : une nouvelle recherche redevient possible.
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(2)
  })

  it('vol unique : l’état est rétabli même après une erreur', async () => {
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(JSON.stringify({ error: 'annuaire muet' }), { status: 422 })
      return new Response(JSON.stringify({ stations: [], search: [] }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.text()).toContain('annuaire muet')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(2)
  })

  it('adresse ses requêtes sous le préfixe absolu reçu par la prop `base`', async () => {
    // IMPORTANT 6 de la revue finale. Cette vue appelait `api.get('./api/data')`
    // en relatif, donc resolu contre l'URL du navigateur et non contre quoi que
    // ce soit que le contrat garantisse : sur `/plugins/radio` (sans slash
    // final, forme que le routeur du shell acceptait aussi), `./api/data`
    // resolvait vers `/plugins/api/data` — que le coeur interprete comme le
    // plugin « api » : 404, table vide et tous les boutons en echec.
    const spy = reponses({ stations: [{ preset: 1, name: 'A', url: 'http://a' }], search: [] })
    vi.stubGlobal('fetch', spy)
    // Volontairement un prefixe qui n'est **pas** `/plugins/radio/` : le nom
    // sous lequel un plugin est servi vient de `plugins.toml`, donc du
    // deploiement. Ce test echouerait si la vue reconstruisait son propre nom
    // au lieu d'honorer le prefixe recu.
    const w = mount(RadioAdmin, {
      props: { catalog: CATALOGUE, base: '/plugins/tuner/' },
    })
    await flushPromises()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    // Toutes les requetes, GET comme PUT, partent sur l'URL absolue.
    expect(spy.mock.calls.length).toBeGreaterThan(1)
    for (const appel of spy.mock.calls) {
      expect(appel[0]).toBe('/plugins/tuner/api/data')
    }
  })

  // --- Garde de chargement en echec (CRITICAL 1 de la revue finale) ---
  //
  // L'ancienne page terminait son `catch` de chargement par
  // `document.querySelectorAll('button').forEach(b => b.disabled = true)`.
  // Sans ce garde, un GET en echec laisse `stations` vide et « Enregistrer »
  // actif : le PUT `{op:'save', stations: []}` est accepte par
  // `Stations::validate` (qui itere sur un vecteur vide) et **ecrase
  // stations.toml** — toutes les preselections perdues, sans confirmation.
  // Atteignable : le plugin sert les requetes d'admin strictement en serie
  // avec un budget annuaire de 4 s contre le plafond de 5 s du coeur, donc un
  // chargement concurrent d'une recherche peut faire echouer le GET alors
  // qu'un PUT ulterieur reussira (un redemarrage du plugin entre les deux
  // produit le meme effet).
  function chargementEnEchec() {
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(null, { status: 204 })
      // Le GET initial echoue : `api.get` leve sur un statut non-ok.
      return new Response('indisponible', { status: 503 })
    })
    vi.stubGlobal('fetch', spy)
    return spy
  }

  it('chargement en échec : les trois boutons d’action sont désactivés', async () => {
    const spy = chargementEnEchec()
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(w.text()).toContain('Erreur : ')
    for (const marqueur of ['[data-add]', '[data-save]', '[data-search]']) {
      expect((w.find(marqueur).element as HTMLButtonElement).disabled, marqueur).toBe(true)
    }
    // Rien n'est parti : le chargement a echoue, aucune ecriture ne doit
    // avoir eu lieu du seul fait du montage.
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('chargement en échec : un clic sur « Enregistrer » n’émet aucune requête', async () => {
    const spy = chargementEnEchec()
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    // `dispatchEvent` plutot que le `trigger()` de VTU : ce dernier renonce
    // de lui-meme sur un element `disabled`, ce qui ferait passer ce test
    // sans qu'aucune garde ne soit exercee dans le code de la vue. On
    // dispatche donc le clic directement, ce qui appelle bien le gestionnaire
    // `@click` : c'est le **retour anticipe** d'`enregistrer()` qui est
    // teste ici, pas l'etat visuel du bouton (ceinture et bretelles : la
    // protection ne doit pas reposer sur le seul attribut `disabled`).
    w.find('[data-save]').element.dispatchEvent(new Event('click'))
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('chargement en échec : la touche Entrée dans la recherche n’émet aucune requête et garde le message d’erreur', async () => {
    // Correction repliee (revue finale) : `:disabled` sur le bouton
    // « Chercher » ne protege pas `@keydown.enter="chercher"`, qui
    // atteignait encore `chercher()`. Une recherche reussie y ferait
    // `message.value = ''`, effacant le message d'erreur de chargement
    // alors que `chargementEchoue` reste vrai -- la page paraitrait saine
    // alors qu'elle est inerte. `chercher()` doit donc porter le meme
    // retour anticipe qu'`enregistrer()`.
    const spy = chargementEnEchec()
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(w.text()).toContain('Erreur : ')
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-query]').trigger('keydown', { key: 'Enter' })
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    // Le message d'erreur de chargement subsiste : il n'a pas ete efface
    // par un `chercher()` qui aurait quand meme tourne.
    expect(w.text()).toContain('Erreur : ')
  })

  it('ajoute un résultat de recherche dans la table en cours d’édition', async () => {
    const { w } = await monter({
      stations: [],
      search: [{ name: 'FIP', url: 'http://fip', codec: 'MP3', bitrate: 128, country: 'FR' }],
    })
    await w.find('[data-add-result]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(1)
    expect((w.find('[data-station-name]').element as HTMLInputElement).value).toBe('FIP')
  })
})
