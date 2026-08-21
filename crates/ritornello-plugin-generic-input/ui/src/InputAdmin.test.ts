import { Select } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import InputAdmin from './InputAdmin.vue'

const CATALOGUE = {
  device_label: 'Périphérique', btn_refresh: 'Rafraîchir', col_action: 'Action', col_code: 'Code',
  btn_learn: 'Apprendre', btn_clear: 'Effacer', btn_save: 'Enregistrer', btn_cancel: 'Annuler',
  btn_load_preset: 'Charger', btn_import: 'Importer', btn_export: 'Exporter',
  preset_label: 'Preset', learn_timeout: 'Délai dépassé',
  dlg_learn_title: 'Apprentissage d’une touche',
  dlg_learn_desc: 'Appuyez sur une touche du périphérique « {device} »…',
  learn_append_label: 'Ajouter aux codes existants',
  saved: 'Enregistré', save_error: 'Échec : ', load_error: 'Erreur : ', no_device: 'Aucun périphérique',
  conflict_code: 'le code {code} est déjà affecté à {action}',
  conflict_dup: 'le code {code} est saisi deux fois',
  save_conflicts: 'Corrigez les codes en double avant d’enregistrer',
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

const SONDAGE_MS = 300

function stub(data: () => unknown) {
  const spy = vi.fn(async (_u: string, init?: RequestInit) =>
    init?.method === 'PUT'
      ? new Response(null, { status: 204 })
      : new Response(JSON.stringify(data()), { status: 200 }),
  )
  vi.stubGlobal('fetch', spy)
  return spy
}

// La popin d'apprentissage part dans un portail vers `document.body` (comme
// tout `Dialog` du kit) : `wrapper.find()` ne la voit jamais. D'ou le
// `attachTo` sur tous les montages, la recherche dans le document, et le
// nettoyage de `document.body` entre les tests (un portail survit au
// demontage de son wrapper).
function monterVue(base = BASE) {
  return mount(InputAdmin, { props: { catalog: CATALOGUE, base }, attachTo: document.body })
}

const dansPopin = (selecteur: string) => document.body.querySelector(selecteur)
const popin = () => dansPopin('[data-dlg-learn]')

/** Les `op` des PUT emis, dans l'ordre. */
const ops = (spy: ReturnType<typeof stub>) =>
  spy.mock.calls
    .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
    .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)

async function monter(data: () => unknown = () => DATA) {
  const spy = stub(data)
  const w = monterVue()
  await flushPromises()
  return { w, spy }
}

async function cocherAjouter() {
  const boite = dansPopin('[data-learn-append]') as HTMLInputElement
  boite.checked = true
  boite.dispatchEvent(new Event('change'))
  await flushPromises()
}

// La ligne d'une action, reperee par le libelle de sa **premiere** cellule :
// un `r.text().includes(libelle)` matcherait aussi une ligne dont le message
// de conflit nomme cette action.
const ligneAction = (w: ReturnType<typeof monterVue>, libelle: string) =>
  w.findAll('[data-action-row]').find((r) => r.findAll('td')[0]!.text() === libelle)!

// Scenario complet d'apprentissage sous faux timers : ouverture de la popin
// sur la ligne portant `libelle`, case « ajouter » cochee ou non, puis code
// capte par le serveur au sondage suivant.
async function apprendreEtCapter(libelle: string, code: number, ajouter = false) {
  vi.useFakeTimers()
  let captured: number | null = null
  const spy = stub(() => ({ ...DATA, learning: { captured } }))
  const w = monterVue()
  await vi.advanceTimersByTimeAsync(0)
  // `ligneAction` et non un `text().includes` : des que le code capte cree un
  // conflit, le message de la ligne fautive nomme l'autre action, et une
  // recherche sur tout le texte de la ligne renverrait la mauvaise.
  const ligne = () => ligneAction(w, libelle)
  await ligne().find('[data-learn]').trigger('click')
  await vi.advanceTimersByTimeAsync(0)
  if (ajouter) await cocherAjouter()
  captured = code
  await vi.advanceTimersByTimeAsync(SONDAGE_MS)
  return { w, spy, ligne, valeur: () => (ligne().find('input').element as HTMLInputElement).value }
}

// Table saine chargee (« Muet » porte le code 9), puis ce meme 9 saisi a la
// main dans « Veille » : deux lignes en conflit, exactement ce que le serveur
// refuserait a l'enregistrement (`duplicate_code`).
async function conflitEntreDeuxLignes() {
  const { w, spy } = await monter()
  await ligneAction(w, 'Veille').find('input').setValue('9')
  return { w, spy }
}

describe('InputAdmin', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    document.body.innerHTML = ''
  })
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
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    // La consigne « appuyez sur une touche » et l'annulation vivent desormais
    // dans la popin, pas dans le bandeau du bas.
    expect(popin()!.textContent).toContain('Appuyez sur une touche')
    expect(dansPopin('[data-learn-cancel]')).not.toBeNull()
    expect(
      JSON.parse(String((spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')![1] as RequestInit).body)),
    ).toEqual({ op: 'learn', device: 'mce' })
    captured = 42
    await vi.advanceTimersByTimeAsync(SONDAGE_MS)
    expect((muet.find('input').element as HTMLInputElement).value).toBe('42')
    // Le sondage s'arrete, `cancel_learn` est emis, et la popin se referme.
    expect(ops(spy)).toContain('cancel_learn')
    expect(popin()).toBeNull()
  })

  it('apprentissage : la popin s’ouvre et son titre nomme l’action apprise', async () => {
    vi.useFakeTimers()
    stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    const contenu = popin()!.textContent!
    expect(contenu).toContain('Apprentissage d’une touche')
    // Le libelle traduit de **cette** ligne, pas d'une autre.
    expect(contenu).toContain('Muet')
    expect(contenu).not.toContain('Veille')
    // Et le peripherique courant, nomme par la description.
    expect(contenu).toContain('mce')
  })

  it('apprentissage : case décochée, le code capturé remplace le champ', async () => {
    // « Muet » porte deja le code 9 : sans la case cochee, le code capture
    // prend sa place au lieu de s'y ajouter.
    const { valeur } = await apprendreEtCapter('Muet', 42)
    expect(valeur()).toBe('42')
    expect(popin()).toBeNull()
  })

  it('apprentissage : case cochée, le code capturé s’ajoute au champ', async () => {
    const { valeur } = await apprendreEtCapter('Muet', 42, true)
    expect(valeur()).toBe('9, 42')
    expect(popin()).toBeNull()
  })

  it('apprentissage : case cochée, un code déjà présent laisse le champ intact', async () => {
    // Pas de « 9, 9 » : le serveur refuserait la table entiere
    // (`duplicate_code`), et l'utilisateur n'a rien demande de plus.
    const { valeur } = await apprendreEtCapter('Muet', 9, true)
    expect(valeur()).toBe('9')
  })

  it('apprentissage : case cochée sur une ligne sans code, le champ vaut le code seul', async () => {
    // « Veille » ne porte aucun code : l'ajout doit donner « 42 » et non
    // « , 42 ». Cas specifie par `appliquerCode` (`!champ.trim()`) mais que les
    // autres tests, tous sur « Muet » (deja pourvu du code 9), ne couvraient
    // pas.
    const { valeur } = await apprendreEtCapter('Veille', 42, true)
    expect(valeur()).toBe('42')
  })

  it('apprentissage : la case « ajouter » revient décochée à chaque ouverture', async () => {
    const { ligne } = await apprendreEtCapter('Muet', 42, true)
    expect(popin()).toBeNull()
    await ligne().find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect((dansPopin('[data-learn-append]') as HTMLInputElement).checked).toBe(false)
  })

  it('apprentissage : le bouton « Annuler » de la popin annule la session serveur', async () => {
    vi.useFakeTimers()
    const spy = stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(ops(spy)).not.toContain('cancel_learn')
    ;(dansPopin('[data-learn-cancel]') as HTMLButtonElement).click()
    await vi.advanceTimersByTimeAsync(0)
    expect(ops(spy)).toContain('cancel_learn')
    expect(popin()).toBeNull()
  })

  it('apprentissage : une annulation en échec réseau referme quand même la popin', async () => {
    // L'annulation est desormais le geste courant (bouton, croix, Échap,
    // voile) et son PUT `cancel_learn` peut echouer. La popin doit se refermer
    // et le sondage mourir malgre tout : `arreterApprentissage` fait les deux
    // avant tout `await`.
    vi.useFakeTimers()
    const spy = vi.fn(async (_u: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        if (JSON.parse(String(init.body)).op === 'cancel_learn') throw new Error('réseau coupé')
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ ...DATA, learning: { captured: null } }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)
    await ligneAction(w, 'Muet').find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    ;(dansPopin('[data-learn-cancel]') as HTMLButtonElement).click()
    await vi.advanceTimersByTimeAsync(0)
    // La popin se referme quand meme : elle ne depend pas de l'aller-retour.
    expect(popin()).toBeNull()
    // Et le sondage est bien mort : plus une seule requete, GET compris, dans
    // la seconde qui suit -- un intervalle survivant sonderait toutes les
    // 300 ms. `ops` ne garde que les PUT, il ne verrait pas ces GET : d'ou le
    // comptage brut des appels, en plus de l'absence de second `cancel_learn`.
    const appels = spy.mock.calls.length
    await vi.advanceTimersByTimeAsync(1_000)
    expect(spy.mock.calls.length).toBe(appels)
    expect(ops(spy)).toEqual(['learn', 'cancel_learn'])
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
    const w = monterVue()
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
    const w = monterVue('/plugins/telecommande/')
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
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)

    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(popin()!.textContent).toContain('Appuyez sur une touche')

    expect(ops(spy)).not.toContain('cancel_learn')

    // Changement de peripherique : « mce » -> « clavier ».
    await w.findAllComponents(Select)[0]!.vm.$emit('update:modelValue', 'clavier')
    await vi.advanceTimersByTimeAsync(0)

    // 1. La session serveur est explicitement annulee.
    expect(ops(spy)).toContain('cancel_learn')
    // 2. L'IHM n'est plus en etat « appuyez sur une touche » pour un
    //    peripherique que personne n'apprend : la popin, qui porte cette
    //    phrase et l'annulation, a disparu.
    expect(popin()).toBeNull()

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
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)

    const muet = () => w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    expect((muet().find('input').element as HTMLInputElement).value).toBe('9')

    await muet().find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(popin()!.textContent).toContain('Appuyez sur une touche')

    // Changement de peripherique : l'annulation (`cancel_learn`) echoue
    // reseau. Sans le correctif, la ligne « Muet » resterait a « 9 »
    // (bindings de « mce ») au lieu d'etre repeuplee pour « clavier ».
    await w.findAllComponents(Select)[0]!.vm.$emit('update:modelValue', 'clavier')
    await vi.advanceTimersByTimeAsync(0)

    expect((muet().find('input').element as HTMLInputElement).value).toBe('5')
  })

  it('apprentissage : abandonne après 30 s avec le message de délai, pas avant', async () => {
    vi.useFakeTimers()
    const spy = stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = monterVue()
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    // A 29 s, le plafond de 30 s n'est pas encore atteint : la popin est
    // toujours ouverte et rien n'a ete annule. Sans cette assertion prise
    // avant l'echeance, le test ne distinguerait pas 30 s d'un delai plus
    // court -- les 10 s d'origine le font echouer ici, ce qui est le but.
    await vi.advanceTimersByTimeAsync(29_000)
    expect(popin()).not.toBeNull()
    expect(ops(spy)).not.toContain('cancel_learn')
    expect(w.text()).not.toContain('Délai dépassé')
    // A 31 s, l'echeance est franchie : la popin se referme et le message de
    // delai s'affiche dans le bandeau du bas, desormais visible.
    await vi.advanceTimersByTimeAsync(2_000)
    expect(popin()).toBeNull()
    expect(ops(spy)).toContain('cancel_learn')
    expect(w.text()).toContain('Délai dépassé')
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

  it('validation : un code déjà porté par une autre action met les deux lignes en erreur', async () => {
    const { w } = await conflitEntreDeuxLignes()
    // L'anneau et la bordure rouges viennent de l'`Input` du kit, qui porte
    // deja `aria-invalid:border-destructive` : l'attribut est tout le signal.
    expect(ligneAction(w, 'Muet').find('input').attributes('aria-invalid')).toBe('true')
    expect(ligneAction(w, 'Veille').find('input').attributes('aria-invalid')).toBe('true')
    // Chaque ligne nomme l'**autre** action, par son libelle traduit et jamais
    // par sa cle i18n.
    const muet = ligneAction(w, 'Muet').find('[data-conflict]').text()
    const veille = ligneAction(w, 'Veille').find('[data-conflict]').text()
    expect(muet).toContain('Veille')
    expect(muet).not.toContain('act_power')
    expect(veille).toContain('Muet')
    expect(veille).not.toContain('act_mute')
  })

  it('validation : le message de conflit nomme le code fautif', async () => {
    const { w } = await conflitEntreDeuxLignes()
    expect(ligneAction(w, 'Veille').find('[data-conflict]').text()).toBe('le code 9 est déjà affecté à Muet')
  })

  it('validation : un doublon interne au champ est signalé sans nommer d’action', async () => {
    const { w } = await monter()
    await ligneAction(w, 'Muet').find('input').setValue('9, 9')
    const message = ligneAction(w, 'Muet').find('[data-conflict]')
    expect(message.exists()).toBe(true)
    expect(message.text()).toBe('le code 9 est saisi deux fois')
    // Aucune autre action n'est en cause : le message ne doit citer personne.
    expect(message.text()).not.toContain('Muet')
  })

  it('validation : tant qu’un conflit existe, « Enregistrer » est désactivé et n’émet rien', async () => {
    const { w, spy } = await conflitEntreDeuxLignes()
    const enregistrer = w.find('[data-save]')
    expect(enregistrer.attributes('disabled')).toBeDefined()
    // Un bouton grise sans phrase n'explique rien.
    expect(w.find('[data-save-blocked]').text()).toBe(CATALOGUE.save_conflicts)
    await enregistrer.trigger('click')
    await flushPromises()
    expect(ops(spy)).toEqual([])
  })

  it('validation : effacer le code fautif retire l’erreur et réactive « Enregistrer »', async () => {
    const { w } = await conflitEntreDeuxLignes()
    // Le conflit existe **avant** l'effacement : sans cette assertion, le test
    // passerait tout aussi bien si `[data-conflict]` n'etait jamais rendu.
    expect(w.findAll('[data-conflict]')).toHaveLength(2)
    await ligneAction(w, 'Veille').find('[data-clear]').trigger('click')
    expect(w.findAll('[data-conflict]')).toHaveLength(0)
    expect(w.find('[data-save]').attributes('disabled')).toBeUndefined()
    expect(w.find('[data-save-blocked]').exists()).toBe(false)
  })

  it('validation : un code arrivé par apprentissage allume la validation comme une frappe', async () => {
    // La couture entre la popin et la validation a chaud : `appliquerCode`
    // ecrit dans le meme `codes`, donc le `computed` doit recalculer. Rien ne
    // le tenait -- tous les tests de conflit passaient par `setValue`, tous
    // ceux d'apprentissage par un code libre. « Muet » porte deja 9 : le
    // capturer sur « Veille » doit mettre les deux lignes en erreur.
    const { w, ligne } = await apprendreEtCapter('Veille', 9)
    expect(w.findAll('[data-conflict]')).toHaveLength(2)
    // La ligne apprise nomme l'autre action, et l'autre la nomme en retour.
    // `ligne()` vient du scenario : c'est aussi ce qui tient sa recherche de
    // ligne sur la premiere cellule -- un `text().includes('Veille')` ramenerait
    // ici la ligne « Muet », dont le message de conflit nomme « Veille ».
    expect(ligne().find('[data-conflict]').text()).toContain('Muet')
    expect(ligneAction(w, 'Muet').find('[data-conflict]').text()).toContain('Veille')
    expect(w.find('[data-save]').attributes('disabled')).toBeDefined()
  })

  it('validation : une table saine chargée du serveur n’affiche aucun conflit', async () => {
    // Garde contre un faux positif au montage : les 22 champs vides ne sont
    // pas 22 fois le meme code.
    const { w } = await monter()
    expect(w.findAll('[data-conflict]')).toHaveLength(0)
    expect(w.find('[data-save]').attributes('disabled')).toBeUndefined()
  })
})
