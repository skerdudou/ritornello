import { Select } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import InputAdmin from './InputAdmin.vue'

const CATALOGUE = {
  device_label: 'Périphérique', btn_refresh: 'Rafraîchir', col_action: 'Action', col_code: 'Code',
  btn_learn: 'Apprendre', btn_clear: 'Effacer', btn_save: 'Enregistrer', btn_cancel: 'Annuler',
  btn_load_preset: 'Charger', btn_import: 'Importer', btn_export: 'Exporter',
  preset_label: 'Preset', learning_msg: 'Appuyez sur une touche', learn_timeout: 'Délai dépassé',
  saved: 'Enregistré', save_error: 'Échec : ', load_error: 'Erreur : ', no_device: 'Aucun périphérique',
  act_mute: 'Muet', act_power: 'Veille',
}

const DATA = {
  devices: ['mce', 'clavier'],
  bindings: { devices: [{ name: 'mce', bindings: [{ code: 9, cmd: 'Mute' }] }] },
  presets: ['mce', 'keyboard'],
  learning: null as { captured: number | null } | null,
}

// Prefixe absolu que le shell passe par la prop `base` (requise) : c'est le
// contrat, cette vue ne connait pas le nom sous lequel elle est servie.
const BASE = '/plugins/generic-input/'

function stub(data: () => unknown) {
  const spy = vi.fn(async (_u: string, init?: RequestInit) =>
    init?.method === 'PUT'
      ? new Response(null, { status: 204 })
      : new Response(JSON.stringify(data()), { status: 200 }),
  )
  vi.stubGlobal('fetch', spy)
  return spy
}

async function monter(data: () => unknown = () => DATA) {
  const spy = stub(data)
  const w = mount(InputAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, spy }
}

describe('InputAdmin', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(() => vi.useRealTimers())

  it('liste les périphériques, les presets et les 23 actions', async () => {
    const { w } = await monter()
    expect(w.findAll('[data-action-row]')).toHaveLength(23)
    expect(w.find('[data-device-select]').exists()).toBe(true)
  })

  it('préremplit les codes du périphérique sélectionné', async () => {
    const { w } = await monter()
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    expect((muet.find('input').element as HTMLInputElement).value).toBe('9')
  })

  it('efface un code sans toucher au serveur', async () => {
    const { w, spy } = await monter()
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-clear]').trigger('click')
    expect((muet.find('input').element as HTMLInputElement).value).toBe('')
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('enregistre la table complète du périphérique courant', async () => {
    const { w, spy } = await monter()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')!
    const corps = JSON.parse(String((put[1] as RequestInit).body))
    expect(corps.op).toBe('save')
    expect(corps.bindings.devices.find((d: { name: string }) => d.name === 'mce').bindings).toEqual([
      { code: 9, cmd: 'Mute' },
    ])
  })

  it('apprentissage : sonde toutes les 300 ms puis remplit le code capturé', async () => {
    vi.useFakeTimers()
    let captured: number | null = null
    const spy = stub(() => ({ ...DATA, learning: { captured } }))
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(w.text()).toContain('Appuyez sur une touche')
    // Le bouton « Annuler » n'est visible que pendant l'apprentissage.
    expect(w.text()).toContain('Annuler')
    expect(
      JSON.parse(String((spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')![1] as RequestInit).body)),
    ).toEqual({ op: 'learn', device: 'mce' })
    captured = 42
    await vi.advanceTimersByTimeAsync(300)
    expect((muet.find('input').element as HTMLInputElement).value).toBe('42')
    // Le sondage s'arrete et `cancel_learn` est emis, et le bouton « Annuler »
    // redisparait.
    const ops = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    expect(ops).toContain('cancel_learn')
    expect(w.text()).not.toContain('Annuler')
  })

  it('apprentissage : deux déclenchements concurrents sur deux lignes ne créent jamais deux minuteurs', async () => {
    // Mutation testee explicitement (voir rapport, section « Round de
    // correction 1 ») : avec le garde d'origine (fonde uniquement sur
    // `timer`, affecte seulement apres le `await` du PUT `learn`), ce test
    // echoue -- deux PUT `learn` partent, et l'intervalle orphelin peut
    // ecrire le code capture dans la mauvaise ligne.
    vi.useFakeTimers()
    let debloquerLearn: () => void = () => {}
    const learnEnCours = new Promise<void>((r) => (debloquerLearn = r))
    let captured: number | null = null
    const spy = vi.fn(async (_u: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        const corps = JSON.parse(String(init.body))
        if (corps.op === 'learn') await learnEnCours
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ ...DATA, learning: { captured } }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    const veille = w.findAll('[data-action-row]').find((r) => r.text().includes('Veille'))!

    // Deux declenchements sur deux lignes differentes avant que le premier
    // PUT `learn` ne fasse son aller-retour (double-clic, ou clic sur une
    // autre action -- plausible sur un Pi 2 ou l'aller-retour n'est pas
    // instantane).
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    await veille.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)

    // Un seul PUT `learn` a du partir : le garde synchrone doit arreter le
    // second declenchement avant tout appel reseau.
    const putsLearn = spy.mock.calls.filter(
      (c) => (c[1] as RequestInit)?.method === 'PUT' && JSON.parse(String((c[1] as RequestInit).body)).op === 'learn',
    )
    expect(putsLearn).toHaveLength(1)

    captured = 42
    debloquerLearn()
    await vi.advanceTimersByTimeAsync(300)

    // Seule la ligne dont le PUT `learn` est reellement parti (« Muet »)
    // recoit le code capture ; « Veille » -- dont le declenchement a ete
    // refuse par le garde -- reste vide.
    expect((muet.find('input').element as HTMLInputElement).value).toBe('42')
    expect((veille.find('input').element as HTMLInputElement).value).toBe('')
  })

  it('adresse ses requêtes sous le préfixe absolu reçu par la prop `base`', async () => {
    // IMPORTANT 6 de la revue finale. Cette vue appelait `api.get('./api/data')`
    // en relatif, donc resolu contre l'URL du navigateur et non contre quoi que
    // ce soit que le contrat garantisse : sur `/plugins/generic-input` (sans
    // slash final, forme que le routeur du shell acceptait aussi),
    // `./api/data` resolvait vers `/plugins/api/data` — que le coeur interprete
    // comme le plugin « api » : 404, table vide et tous les boutons en echec.
    const spy = stub(() => DATA)
    // Volontairement un prefixe qui n'est **pas** `/plugins/generic-input/` :
    // le nom sous lequel un plugin est servi vient de `plugins.toml`, donc du
    // deploiement. Ce test echouerait si la vue reconstruisait son propre nom
    // au lieu d'honorer le prefixe recu.
    const w = mount(InputAdmin, {
      props: { catalog: CATALOGUE, base: '/plugins/telecommande/' },
    })
    await flushPromises()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.length).toBeGreaterThan(1)
    for (const appel of spy.mock.calls) {
      expect(appel[0]).toBe('/plugins/telecommande/api/data')
    }
  })

  it('apprentissage : changer de périphérique annule la session en cours', async () => {
    // IMPORTANT 2 de la revue finale. L'ancien gestionnaire annulait
    // l'apprentissage au changement de peripherique
    // (`$('dev').onchange = async () => { if (timer) await stopLearn(''); … }`) ;
    // le `watch(device, remplirCodes)` avait perdu cette annulation.
    //
    // Sans elle, l'intervalle continue de sonder alors que la session
    // d'apprentissage du serveur est encore armee sur le peripherique
    // **precedent**, `remplirCodes()` a entre-temps repeuple la table depuis
    // les bindings du **nouveau** peripherique, et la fermeture ecrit le code
    // capture dans la ligne du nouveau peripherique -- que « Enregistrer »
    // persiste ensuite. Meme classe de defaut que la course corrigee en
    // Task 12, dont la correction n'avait pas envisage le changement de
    // peripherique.
    vi.useFakeTimers()
    let captured: number | null = null
    const spy = stub(() => ({ ...DATA, learning: { captured } }))
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await vi.advanceTimersByTimeAsync(0)

    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(w.text()).toContain('Appuyez sur une touche')

    const opsAvant = () =>
      spy.mock.calls
        .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
        .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    expect(opsAvant()).not.toContain('cancel_learn')

    // Changement de peripherique : « mce » -> « clavier ».
    await w.findAllComponents(Select)[0]!.vm.$emit('update:modelValue', 'clavier')
    await vi.advanceTimersByTimeAsync(0)

    // 1. La session serveur est explicitement annulee.
    expect(opsAvant()).toContain('cancel_learn')
    // 2. L'IHM n'est plus en etat « appuyez sur une touche » pour un
    //    peripherique que personne n'apprend.
    expect(w.text()).not.toContain('Appuyez sur une touche')
    expect(w.text()).not.toContain('Annuler')

    // 3. Le code que le peripherique precedent aurait fini par capturer ne
    //    doit atterrir dans aucune ligne de la table du nouveau peripherique.
    captured = 42
    await vi.advanceTimersByTimeAsync(1_000)
    const codes = w
      .findAll('[data-action-row]')
      .map((r) => (r.find('input').element as HTMLInputElement).value)
    expect(codes.every((v) => v === '')).toBe(true)
  })

  it('apprentissage : la table est repeuplée même si l’annulation réseau échoue', async () => {
    // Correction repliee (revue finale) : `watch(device, …)` faisait
    // `await arreterApprentissage('')` sans filet. Si `fetch` rejette
    // (reseau coupe), la rejection non rattrapee sautait `remplirCodes()`,
    // et les codes du peripherique **precedent** restaient affiches sous le
    // nouveau -- exactement la classe de defaut que ce watcher venait
    // corriger, dans la branche d'echec reseau.
    vi.useFakeTimers()
    const bindings = {
      devices: [
        { name: 'mce', bindings: [{ code: 9, cmd: 'Mute' }] },
        { name: 'clavier', bindings: [{ code: 5, cmd: 'Mute' }] },
      ],
    }
    const spy = vi.fn(async (_u: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        const corps = JSON.parse(String(init.body))
        if (corps.op === 'cancel_learn') throw new Error('réseau coupé')
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ ...DATA, bindings, learning: null }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await vi.advanceTimersByTimeAsync(0)

    const muet = () => w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    expect((muet().find('input').element as HTMLInputElement).value).toBe('9')

    await muet().find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(w.text()).toContain('Appuyez sur une touche')

    // Changement de peripherique : l'annulation (`cancel_learn`) echoue
    // reseau. Sans le correctif, la ligne « Muet » resterait a « 9 »
    // (bindings de « mce ») au lieu d'etre repeuplee pour « clavier ».
    await w.findAllComponents(Select)[0]!.vm.$emit('update:modelValue', 'clavier')
    await vi.advanceTimersByTimeAsync(0)

    expect((muet().find('input').element as HTMLInputElement).value).toBe('5')
  })

  it('apprentissage : abandonne après 10 s avec le message de délai, pas avant', async () => {
    vi.useFakeTimers()
    stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    // A 9 s, le plafond de 10 s n'est pas encore atteint : l'apprentissage
    // doit etre toujours actif. Sans cette assertion prise avant l'echeance,
    // le test ne distinguerait pas un `DELAI_MS` de 10 s d'un `DELAI_MS`
    // errone (1 s ou 5 s par exemple) : les deux finiraient par afficher le
    // message de delai a 10,5 s.
    await vi.advanceTimersByTimeAsync(9_000)
    expect(w.text()).toContain('Annuler')
    expect(w.text()).not.toContain('Délai dépassé')
    await vi.advanceTimersByTimeAsync(1_500)
    expect(w.text()).toContain('Délai dépassé')
    expect(w.text()).not.toContain('Annuler')
  })

  it('sans périphérique, prévient et n’émet aucune opération (save)', async () => {
    const { w, spy } = await monter(() => ({ ...DATA, devices: [] }))
    expect(w.text()).toContain('Aucun périphérique')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('sans périphérique, prévient et n’émet aucune opération (learn)', async () => {
    const { w, spy } = await monter(() => ({ ...DATA, devices: [] }))
    const uneLigne = w.findAll('[data-action-row]')[0]!
    await uneLigne.find('[data-learn]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('sans périphérique, prévient et n’émet aucune opération (load_preset)', async () => {
    const { w, spy } = await monter(() => ({ ...DATA, devices: [] }))
    const bouton = w.findAll('button').find((b) => b.text() === CATALOGUE.btn_load_preset)!
    await bouton.trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('sans périphérique, prévient et n’émet aucune opération (import)', async () => {
    const { w, spy } = await monter(() => ({ ...DATA, devices: [] }))
    const fichier = new File(['[[bindings]]\ncode = 1\ncmd = "Mute"\n'], 'p.toml')
    const input = w.find('[data-import]').element as HTMLInputElement
    Object.defineProperty(input, 'files', { value: [fichier] })
    await w.find('[data-import]').trigger('change')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('sans périphérique, prévient et n’émet aucune opération (export)', async () => {
    const { w, spy } = await monter(() => ({ ...DATA, devices: [] }))
    const bouton = w.findAll('button').find((b) => b.text() === CATALOGUE.btn_export)!
    await bouton.trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Aucun périphérique')
  })

  it('rafraîchir envoie `rescan` puis recharge', async () => {
    const { w, spy } = await monter()
    await w.find('[data-refresh]').trigger('click')
    await flushPromises()
    const ops = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    expect(ops).toEqual(['rescan'])
  })

  it('revalide le preset sélectionné quand il disparaît de la nouvelle liste', async () => {
    let presets = ['mce', 'keyboard']
    const { w } = await monter(() => ({ ...DATA, presets }))
    const presetSelect = w.findAllComponents(Select)[1]!
    expect(presetSelect.props('modelValue')).toBe('mce')
    presets = ['keyboard'] // « mce » disparait de la liste servie
    await w.find('[data-refresh]').trigger('click')
    await flushPromises()
    expect(presetSelect.props('modelValue')).toBe('keyboard')
  })

  it('importe un fichier `.toml` en le confiant au serveur', async () => {
    const { w, spy } = await monter()
    const fichier = new File(['[[bindings]]\ncode = 1\ncmd = "Mute"\n'], 'p.toml')
    const input = w.find('[data-import]').element as HTMLInputElement
    Object.defineProperty(input, 'files', { value: [fichier] })
    await w.find('[data-import]').trigger('change')
    await vi.waitFor(() =>
      expect(
        spy.mock.calls.some(
          (c) =>
            (c[1] as RequestInit)?.method === 'PUT' &&
            JSON.parse(String((c[1] as RequestInit).body)).op === 'import_preset',
        ),
      ).toBe(true),
    )
  })
})
