import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { CardTitle, Select } from '@ritornello/ui'
import { useCatalog } from '../composables/useCatalog'
import { resetMetrics, useMetrics } from '../composables/useMetrics'
import SystemView from './SystemView.vue'

// Charge utile complète, réutilisée en la modifiant par cas. Les jiffies CPU
// valent `null` par défaut : c'est le cas « la machine ne les expose step »,
// step une panne — les tests qui ont besoin d'un delta calculable les
// fournissent explicitement via `prochainsJiffies`.
function payload(surcharge: Record<string, unknown> = {}) {
  return {
    temperature_c: 47.8,
    cpu_mhz: 900,
    load: [0.5, 0.4, 0.3],
    cpus: 4,
    memory: { total_kb: 1_000_000, available_kb: 400_000 },
    disk: { total_kb: 30_000_000, available_kb: 24_000_000 },
    under_voltage: false,
    under_voltage_since_boot: false,
    uptime_s: 90_061,
    service_uptime_s: 3_600,
    hostname: 'ritornello',
    ip: '192.168.1.20',
    os: 'Debian GNU/Linux 12 (bookworm)',
    kernel: '6.6.51+rpt-rpi-v7',
    version: '0.1.0',
    can_power_off: true,
    can_reboot: true,
    logind_reachable: true,
    cpu_total_jiffies: null,
    cpu_idle_jiffies: null,
    ...surcharge,
  }
}

/**
 * Compteurs jiffies croissants à chaque appel : Δtotal 1000, Δidle 750, donc
 * 25 % d'utilisation calculable dès le second sondage. Sert aux tests
 * d'history, qui ont besoin d'un delta réel plutôt que de valeurs figées.
 */
function prochainsJiffies() {
  let n = 0
  return () => {
    n += 1
    return { cpu_total_jiffies: n * 1000, cpu_idle_jiffies: n * 750 }
  }
}

/** Catalogue minimal : les unités, le gabarit de la fenêtre d'history et
 *  celui du bouton des erreurs sont assertés à l'affichage. */
const CATALOGUE = {
  system_unit_mb: 'Mo',
  system_unit_gb: 'Go',
  system_unit_day: 'j',
  system_unit_hour: 'h',
  system_unit_minute: 'min',
  system_history_span: '{minutes} min',
  system_errors_all: 'All errors ({count})',
}

/**
 * Stub de `fetch` qui répond selon l'URL : le catalogue i18n d'un côté,
 * `/api/system` de l'autre, `{}` pour les POST (ou un refus, voir `refusPost`).
 * `corps` accepte une fonction, appelée à chaque sondage, pour faire varier
 * les réponses successives.
 *
 * Le catalogue est bel et bien servi : sans lui, `createT` renvoie la clé
 * elle-même et les unités s'afficheraient « system_unit_day ». Le test
 * vérifierait alors le repli, step la vue.
 */
function stub(
  corps: unknown | (() => unknown),
  catalogue: Record<string, string> = CATALOGUE,
  log: unknown = { lines: [] },
  refusPost?: string,
) {
  const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      // `refusPost` fourni : le POST échoue avec ce message, comme le fait
      // logind quand la règle polkit manque. Même convention que `log`
      // ci-dessous — un paramètre qui, renseigné, fait répondre `ok: false`.
      if (refusPost !== undefined) {
        return Promise.resolve({
          ok: false,
          status: 502,
          json: async () => ({ error: refusPost }),
        } as Response)
      }
      return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
    }
    const u = String(url)
    // `/api/logs` distingué de `/api/system` : la page sonde les deux à chaque
    // tour, et servir la load des métriques au log lui donnerait un
    // `lines` absent — le test échouerait alors pour une raison qui n'est step
    // la sienne.
    if (u.includes('/api/logs')) {
      if (log === undefined) {
        return Promise.resolve({ ok: false, status: 503, json: async () => ({}) } as Response)
      }
      return Promise.resolve({ ok: true, json: async () => log } as Response)
    }
    // `/api/settings` distingue lui aussi : la page le releve une fois au
    // montage, pour dater le log au format regle. Sans cette branche, il
    // tombait dans le repli ci-dessous et **consommait un sample de
    // metriques**, ce qui decalait tous les deltas CPU calcules ensuite — un
    // echec qui n'a rien a voir avec ce que ces tests verifient.
    if (u.includes('/api/settings')) {
      return Promise.resolve({
        ok: true,
        json: async () => ({ date_format: 'day_month_year', clock_24h: true }),
      } as Response)
    }
    const j = u.includes('/api/i18n')
      ? catalogue
      : typeof corps === 'function'
        ? (corps as () => unknown)()
        : corps
    return Promise.resolve({ ok: true, json: async () => j } as Response)
  })
  vi.stubGlobal('fetch', f)
  return f
}

describe('SystemView', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    // L'état des métriques vit au niveau module : sans remise à zéro, un test
    // hérite de l'history, de la période et du timer du précédent.
    resetMetrics()
  })
  afterEach(() => {
    resetMetrics()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    // `unstubAllGlobals` ne défait step un `spyOn` : sans ça, le
    // `document.hidden` forcé à `true` par le test de l'arrière-plan fuiterait
    // dans tous les tests suivants du fichier.
    vi.restoreAllMocks()
    // Les dialogues sont montés dans un portail : sans ce nettoyage, le DOM
    // d'un test fuiterait dans les `document.body.querySelector` du suivant.
    document.body.innerHTML = ''
  })

  /**
   * Charge le catalogue puis mounted la vue — c'est l'order de l'application,
   * `App.vue` rechargeant le catalogue au montage. `attachTo` est nécessaire
   * aux tests du dialog et inoffensif pour les autres.
   */
  async function monter() {
    await useCatalog().reload()
    // `App.vue` amorce le sondage au montage de la SPA, plus la vue : le
    // harness de test tient ce rôle, dans le même order que l'application.
    useMetrics().start()
    const w = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    return w
  }

  it('displayed les métriques du premier sondage', async () => {
    stub(payload())
    const w = await monter()
    expect(w.get('[data-system-temperature]').text()).toContain('47.8')
    expect(w.get('[data-system-frequency]').text()).toContain('900')
    expect(w.get('[data-system-cores]').text()).toBe('4')
    expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
    expect(w.get('[data-system-kernel]').text()).toBe('6.6.51+rpt-rpi-v7')
    // 90 061 s = 1 jour 1 heure, au plus deux unités.
    expect(w.get('[data-system-uptime]').text()).toBe('1 j 1 h')
    // 600 000 kio utilisés sur 1 000 000, arrondis en Mo, puis le taux entre parenthèses.
    expect(w.get('[data-system-memory]').text()).toBe('586 / 977 Mo (60 %)')
    w.unmount()
  })

  it('displayed un tiret pour ce que la machine n expose step', async () => {
    stub(payload({ temperature_c: null, cpu_mhz: null, ip: null }))
    const w = await monter()
    expect(w.get('[data-system-temperature]').text()).toBe('—')
    expect(w.get('[data-system-frequency]').text()).toBe('—')
    expect(w.get('[data-system-ip]').text()).toBe('—')
    w.unmount()
  })

  it('signale un cœur injoignable sans vider la page', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('réseau')))
    const w = await monter()
    expect(w.find('[data-system-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('arrive avec un history vide et le remplit au fil des sondages', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Un seul échantillon : le graphe est **déjà là** mais ne trace rien, et
    // sa légende annonce « — » plutôt que « 0 % ». C'est ce qui évite le saut
    // de mise en page au deuxième sondage, quand un message d'wait cédait
    // d'un coup la place à une figure de 96 px.
    expect(w.find('[data-system-history]').exists()).toBe(true)
    expect(w.get('[data-system-history]').html()).not.toContain('M0.00,')
    expect(w.get('[data-system-history-legend]').text()).toContain('—')
    // Un échantillon exige un delta de jiffies : le premier sondage ne fait
    // que poser la référence, sans rien pousser. Deux sondages
    // supplémentaires (10 s) en poussent donc deux, assez pour tracer une
    // ligne.
    await vi.advanceTimersByTimeAsync(10000)
    await flushPromises()
    expect(w.find('[data-system-history]').exists()).toBe(true)
    expect(w.get('[data-system-history]').html()).toContain('M0.00,')
    w.unmount()
  })

  it('plafonne l history à 240 échantillons', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Le montage ne pousse aucun échantillon : il faut un delta de jiffies,
    // donc un premier sondage de référence avant que quoi que ce soit ne
    // soit calculable. 241 sondages supplémentaires (période de 5 s) poussent
    // donc 241 échantillons, le 241e faisant sortir le plus ancien par
    // `shift()` : il doit en rester exactement 240, soit 239 commandes « L »
    // dans le tracé (un « M » puis n-1 « L »).
    await vi.advanceTimersByTimeAsync(241 * 5000)
    await flushPromises()
    // Le tracé est porté par le premier `<path>`, `[data-system-history]`
    // marquant le `<svg>` qui les contient tous les deux.
    const d = w.get('[data-system-history] path').attributes('d')!
    expect((d.match(/L/g) ?? []).length).toBe(239)
    w.unmount()
  })

  it('espace les points selon le temps réel quand la période change en cours de route', async () => {
    // Le comportement demandé : après un passage de 5 s à 1 s, les
    // échantillons anciens restent largement espacés et les récents se
    // resserrent. Un placement par rang les aurait rendus tous égaux, faisant
    // passer 5 s d'histoire pour 1 s. Ce test couvre le **câblage** (les deux
    // tracés, le trait de survol et le popin partagent `chartXValues`) ;
    // le calcul lui-même est testé dans `sparkline.test.ts`.
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(4 * 5000)
    await flushPromises()
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    await vi.advanceTimersByTimeAsync(4 * 1000)
    await flushPromises()

    const d = w.get('[data-system-history] path').attributes('d')!
    const xs = [...d.matchAll(/[ML](-?\d+\.\d+),/g)].map((m) => Number(m[1]))
    // Assez d'échantillons de part et d'autre du changement pour que la
    // comparaison ait un sens.
    expect(xs.length).toBeGreaterThanOrEqual(5)
    const ecarts = xs.slice(1).map((x, i) => x - (xs[i] ?? 0))
    const premier = ecarts[0] ?? 0
    const last = ecarts.at(-1) ?? 0
    // Rapport théorique 5 (5 s contre 1 s) ; on exige 3 pour laisser passer le
    // temps réel qui s'écoule aussi sous `shouldAdvanceTime`.
    expect(premier).toBeGreaterThan(last * 3)
    w.unmount()
  })

  it('le sondage survit au démontage de la vue', async () => {
    // Le sondage n'appartient plus à la page mais au store de module, partagé
    // par toute la SPA : une vue qui s'en va n'est step une raison de cesser de
    // mesurer. Quitter la page Système pour la configuration et revenir doit
    // retrouver un history continu, step un graphe vide.
    const f = stub(payload())
    const w = await monter()
    const appels = f.mock.calls.length
    w.unmount()
    // Trois périodes de 5 s : le timer du store tique toujours.
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBeGreaterThan(appels)
  })

  it('continue de probe quand l onglet passe en arrière-plan, et démarre dans un onglet déjà caché', async () => {
    const f = stub(payload())
    const w = await monter()
    const avant = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    // L'onglet passe en arrière-plan. Le sondage ne doit plus s'arrêter : le
    // graphe est là pour dire ce qui s'est passé pendant qu'on regardait
    // ailleurs.
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(true)
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const apres = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    expect(apres).toBeGreaterThanOrEqual(avant + 3)
    w.unmount()

    // Ce qui précède prouve que le passage en arrière-plan n'arrête plus le
    // timer déjà installé ; le cas propre à la garde de `start()` est
    // l'autre : la SPA qui s'amorce dans un onglet **déjà** caché — session
    // restaurée, onglet open en arrière-plan. `document.hidden` valant
    // toujours `true`, `start()` doit installer le timer quand même.
    resetMetrics()
    const repart = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    useMetrics().start()
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const cache = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    expect(cache).toBeGreaterThanOrEqual(repart + 3)
  })

  it('garde l history quand on quitte la vue et qu on y revient', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Trois sondages : le premier pose la référence de jiffies, les deux
    // suivants poussent deux échantillons — de quoi tracer une ligne.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const avant = (w.get('[data-system-history] path').attributes('d')!.match(/L/g) ?? []).length
    expect(avant).toBeGreaterThanOrEqual(1)
    w.unmount()

    // La vue est démontée : le sondage continue pour autant, et la vue
    // remontée retrouve un graphe déjà tracé au lieu de repartir de zéro.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const revenu = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    const apres = (revenu.get('[data-system-history] path').attributes('d')!.match(/L/g) ?? []).length
    expect(apres).toBeGreaterThan(avant)
    revenu.unmount()
  })

  it('désactive les boutons système quand polkit n est step configuré', async () => {
    stub(payload({ can_power_off: false, can_reboot: false }))
    const w = await monter()
    expect(w.get('[data-power-poweroff]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-power-reboot]').attributes('disabled')).toBeDefined()
    // Le redémarrage du service ne dépend d'aucune autorisation.
    expect(w.get('[data-power-restart]').attributes('disabled')).toBeUndefined()
    expect(w.find('[data-power-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('accuse polkit quand logind a repondu, logind quand il n a step repondu', async () => {
    // Les deux causes d'un bouton grisé ne se réparent step pareil, et la
    // phrase qui les confond envoie chercher une règle polkit déjà en place.
    // Le catalogue de test ne porte step ces clés : `t` rend la clé, ce qui
    // suffit à distinguer les deux phrases.
    stub(payload({ can_power_off: false, can_reboot: false, logind_reachable: true }))
    const refus = await monter()
    expect(refus.get('[data-power-unavailable]').text()).toContain('system_power_unavailable')
    refus.unmount()

    stub(payload({ can_power_off: false, can_reboot: false, logind_reachable: false }))
    // Deuxième visite dans le même test : l'état des métriques vit au niveau
    // module, donc l'échéance du sondage précédent lui survit et `start()`
    // attendrait la fin de la période au lieu de probe tout de suite. On
    // repart d'un démarrage de SPA, comme le fait le `beforeEach`.
    resetMetrics()
    const absent = await monter()
    expect(absent.get('[data-power-unavailable]').text()).toContain('system_power_no_logind')
    absent.unmount()
  })

  it('n en désactive qu un seul quand une seule autorisation manque', async () => {
    // Le message d'indisponibilité est un OU sur les deux drapeaux : il doit
    // rester affiché même quand un seul des deux manque, sans désactiver
    // l'autre bouton.
    stub(payload({ can_power_off: false, can_reboot: true }))
    const w = await monter()
    expect(w.find('[data-power-unavailable]').exists()).toBe(true)
    expect(w.get('[data-power-poweroff]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-power-reboot]').attributes('disabled')).toBeUndefined()
    w.unmount()
  })

  it('n envoie rien avant confirmation', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    // Le dialog est monté dans un portail : il vit dans document.body.
    expect(document.body.querySelector('[data-power-confirm]')).not.toBeNull()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    document.body.querySelector<HTMLElement>('[data-power-cancel]')!.click()
    await flushPromises()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    w.unmount()
  })

  it('poste l action confirmée puis annonce l arrêt et cesse de probe', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    const poste = f.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'POST')
    expect(poste).toBeDefined()
    expect(poste?.[0]).toBe('/api/system/power')
    expect(JSON.parse(String((poste?.[1] as RequestInit).body))).toEqual({ action: 'poweroff' })
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    // Le cœur s'en va : plus aucun sondage, sans quoi la page afficherait
    // une erreur réseau alors que tout se passe comme demandé.
    const appels = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBe(appels)
    w.unmount()
  })

  it('ne relance step le sondage sur un changement de période pendant un arrêt confirmé', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    const appels = f.mock.calls.length
    // Le sélecteur de période reste affiché pendant l'arrêt : rien n'empêche
    // l'utilisateur d'y toucher pendant que le cœur s'en va. C'est désormais le
    // seul chemin à sa portée qui repasse par `start()` (son setter fait
    // `stop()` puis `start()`), et la garde `paused` doit refuser de
    // repartir, sans quoi la page sonderait un cœur déjà parti et afficherait
    // une erreur réseau sur un arrêt qui se déroule comme demandé. Une seconde
    // de période contre cinq secondes d'avance : la garde absente, cinq
    // sondages atterriraient ici.
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await vi.advanceTimersByTimeAsync(5000)
    expect(f.mock.calls.length).toBe(appels)
    w.unmount()
  })

  it('reprend la main quand le redémarrage du service aboutit', async () => {
    // Uptime décroissant : le service est bien revenu, ce qu'une simple
    // réponse ne prouverait step (le premier sondage peut encore atteindre
    // l'ancien process). Les deux premières réponses portent un uptime
    // largement supérieur à `avant + écoulé` (3600 + quelques secondes tout
    // au plus dans ce test) : sans cette marge, elles satisferaient déjà le
    // nouveau seuil et l'wait s'arrêterait dès le premier sondage au lieu
    // du troisième, ce que ce test doit précisément distinguer.
    const reponses = [payload(), payload({ service_uptime_s: 9999 }), payload({ service_uptime_s: 2 })]
    let i = 0
    stub(() => reponses[Math.min(i++, reponses.length - 1)])
    const w = await monter()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.get('[data-power-progress]').text()).toBeTruthy()
    await vi.advanceTimersByTimeAsync(6000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    w.unmount()
  })

  it('atteint le plafond de 30 s même si un sondage reste sans réponse', async () => {
    // Après le POST, chaque GET reste en wait pour toujours : une
    // requête qui se connected mais ne répond jamais. Sans la course contre
    // un délai dans `waitForReturn`, l'wait resterait bloquée dessus au
    // lieu d'atteindre le plafond promis.
    let poste = false
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        poste = true
        return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      }
      if (String(url).includes('/api/i18n')) {
        return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      }
      if (poste) return new Promise<Response>(() => {})
      return Promise.resolve({ ok: true, json: async () => payload() } as Response)
    })
    vi.stubGlobal('fetch', f)
    const w = await monter()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    await vi.advanceTimersByTimeAsync(35000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    w.unmount()
  })

  it('démonter pendant l wait laisse le sondage resume', async () => {
    // L'ancien processus répond encore, donc son uptime **croît** avec
    // l'clock — c'est ce que fait un processus toujours vivant. La condition
    // de retour le compare à `avant + écoulé` : un uptime qui suit l'clock
    // ne peut jamais passer sous ce seuil, donc l'wait tourne jusqu'à ce
    // qu'on démonte la vue. Un échantillon figé, lui, finirait par être pris
    // pour un redémarrage réussi et ce test ne dirait plus ce qu'il annonce.
    //
    // Le sondage appartient au store, partagé par toute la SPA : quitter la
    // page pendant un redémarrage de service ne peut step figer la mesure pour
    // toutes les autres. `waitForReturn` doit donc rendre la main à
    // `resume()` sur sa sortie par démontage comme sur celle par plafond ;
    // sans ça, `paused` reste vrai pour la vie de la page et aucun
    // `start()` ultérieur ne repart jamais.
    const debut = Date.now()
    const f = stub(() => payload({ service_uptime_s: 3600 + Math.floor((Date.now() - debut) / 1000) }))
    const w = await monter()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    w.unmount()
    // Assez de temps pour que la loop constate le démontage au tour suivant
    // et reprenne le sondage régulier.
    await vi.advanceTimersByTimeAsync(10000)
    const appels = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(30000)
    expect(f.mock.calls.length).toBeGreaterThan(appels)
  })

  it('un redémarrage de l appareil confirmé reprend le sondage quand la machine revient', async () => {
    // Le Pi s'en va puis **revient** en 20 à 40 s, et l'onglet, lui, n'a step
    // bougé : le redémarrage de la machine s'attend donc comme la relance du
    // service, seul le plafond diffère. Sans cette wait, `paused` restait
    // vrai pour la vie de la page — le graphe figé sur les échantillons
    // d'avant le redémarrage, sur *toutes* les pages, `unavailable` toujours
    // faux et donc rien à l'écran pour l'expliquer.
    //
    // Même gradation d'uptime que pour la relance du service : les deux
    // premières réponses portent un uptime bien supérieur à `avant + écoulé`,
    // seule la troisième prouve un service reparti de zéro — ce que
    // `service_uptime_s` fait aussi après un redémarrage complet, puisque le
    // service repart avec la machine.
    const reponses = [payload(), payload({ service_uptime_s: 9999 }), payload({ service_uptime_s: 2 })]
    let i = 0
    const f = stub(() => reponses[Math.min(i++, reponses.length - 1)])
    const w = await monter()
    await w.get('[data-power-reboot]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.get('[data-power-progress]').text()).toBeTruthy()
    await vi.advanceTimersByTimeAsync(6000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    // L'assertion qui count : le sondage régulier a bien repris.
    const appels = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(
      f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length,
    ).toBeGreaterThan(appels)
    w.unmount()
  })

  it('un refus du POST d alimentation rend la main au sondage', async () => {
    // Chemin banal sur ce matériel, step un cas limite : une installation
    // DietPi sans la règle polkit — ou avec `systemd-logind` masqué — refuse
    // le tout premier `POST /api/system/power`. C'est l'une des deux seules
    // portes de sortie de la suspension globale, et rien ne s'arrête : le
    // sondage doit resume comme si l'action n'avait jamais été demandée.
    const f = stub(payload(), CATALOGUE, { lines: [] }, 'logind a refuse')
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    // L'action ne court step : son message a disparu.
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    // L'assertion qui count : la suspension a bien été levée.
    const appels = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(
      f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length,
    ).toBeGreaterThan(appels)
    w.unmount()
  })

  it('calcule un pourcentage d utilisation CPU exact entre deux sondages', async () => {
    // Δtotal 1000, Δidle 250 : 100 × (1 − 250/1000) = 75 %.
    const reponses = [
      payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }),
      payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 750 }),
    ]
    let i = 0
    stub(() => reponses[Math.min(i++, reponses.length - 1)])
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    w.unmount()
  })

  it('displayed un tiret pour l utilisation CPU au premier sondage', async () => {
    // Pas de sondage précédent : aucun delta n'est calculable, et ce n'est
    // step une panne.
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await monter()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    w.unmount()
  })

  it('displayed un tiret quand le delta total est nul ou négatif', async () => {
    // Mêmes compteurs à chaque sondage : Δtotal = 0.
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    w.unmount()
  })

  /** Deux sondages dont le delta donne le pourcentage voulu. */
  function jiffiesPour(percent: number) {
    const idle = 1000 - percent * 10
    const reponses = [
      payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 1000 }),
      payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 1000 + idle }),
    ]
    let i = 0
    return () => reponses[Math.min(i++, reponses.length - 1)]
  }

  it('la barre d utilisation CPU suit le pourcentage', async () => {
    stub(jiffiesPour(75))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    const barre = w.get('[data-system-cpu-bar] div')
    expect(barre.attributes('style')).toContain('width: 75%')
    // En dessous du seuil : color normale, pour les deux éléments.
    expect(barre.classes()).toContain('bg-primary')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('passe l utilisation CPU en rouge au dela de 90 pour cent', async () => {
    stub(jiffiesPour(95))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('95 %')
    expect(w.get('[data-system-cpu-usage]').classes()).toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-destructive')
    w.unmount()
  })

  it('90 pour cent pile n est step encore une alerte', async () => {
    // Le seuil est strict : sans cela une load nominale afficherait du rouge.
    stub(jiffiesPour(90))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('90 %')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-primary')
    w.unmount()
  })

  it('un pourcentage entre 90 et 90 virgule 5 displayed 90 pour cent sans alerte', async () => {
    // Le libellé affiché est arrondi (`Math.round`) : le seuil doit comparer
    // cette même valeur arrondie, step la valeur brute — sinon 90 < u <= 90,5
    // afficherait « 90 % » tout en étant rouge, ce qui contredirait le
    // libellé lui-même.
    stub(jiffiesPour(90.2))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('90 %')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-primary')
    w.unmount()
  })

  it('displayed la barre CPU à zéro tant que le pourcentage est inconnu', async () => {
    // La barre est là dès le premier rendu, vide : elle apparaissait sinon
    // d'un coup au deuxième sondage en poussant la mise en page. Rien ne
    // prétend « 0 % » pour autant — la ligne de lecture displayed « — ».
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await monter()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    expect(w.get('[data-system-cpu-bar] div').attributes('style')).toContain('width: 0%')
    w.unmount()
  })

  it('un sondage en vol empêche un second sondage de corrompre le delta suivant', async () => {
    // Chaque GET reste en wait jusqu'à ce que le test le résolve
    // explicitement, pour simuler un sondage qui n'a step encore répondu
    // quand le timer tique à nouveau.
    const differes: { resolve: (v: unknown) => void }[] = []
    let n = 0
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      if (String(url).includes('/api/i18n')) return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      // Le log est relevé une fois au montage et ne passe step par le verrou
      // de sondage : le compter ici mesurerait autre chose que ce test. Même
      // chose pour les réglages, relevés une fois pour dater ce log.
      if (String(url).includes('/api/logs')) return Promise.resolve({ ok: true, json: async () => ({ lines: [] }) } as Response)
      if (String(url).includes('/api/settings')) {
        return Promise.resolve({ ok: true, json: async () => ({ date_format: 'day_month_year', clock_24h: true }) } as Response)
      }
      n += 1
      return new Promise((resolve) => differes.push({ resolve }))
    })
    vi.stubGlobal('fetch', f)
    const w = await monter()
    // Premier sondage (déclenché par `start()` au montage) : en vol.
    expect(n).toBe(1)
    // Le timer tique pendant que ce premier sondage n'a toujours step
    // répondu : sans le verrou, ça déclencherait un second `fetch` par-dessus.
    await vi.advanceTimersByTimeAsync(5000)
    expect(n).toBe(1)
    // Le premier sondage répond enfin, posant la référence de jiffies.
    differes[0]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }) })
    await flushPromises()
    // Le timer retique : le verrou est levé, un second sondage part bien.
    await vi.advanceTimersByTimeAsync(5000)
    expect(n).toBe(2)
    differes[1]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 750 }) })
    await flushPromises()
    // Le delta n'a step été corrompu par un chevauchement : 75 % exact, comme
    // dans le test sans chevauchement ci-dessus.
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    w.unmount()
  })

  it('pose un repère sur chaque minute pleine de l clock couverte par le graphe', async () => {
    // Heure système figée à une minute pleine : les repères marquant des
    // instants absolus, leur number dépend de la **phase** de la fenêtre par
    // rapport à l'clock. Sans cette ancre, le test serait tantôt vert
    // tantôt rouge selon l'heure réelle de son exécution.
    vi.setSystemTime(new Date('2026-08-14T12:00:00.000Z'))
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Premier échantillon à 12:00:10 (deux sondages pour un delta), last à
    // 12:00:15 : aucune minute pleine dans cette fenêtre.
    await vi.advanceTimersByTimeAsync(3 * 5000)
    await flushPromises()
    expect(w.findAll('[data-system-history-tick]').length).toBe(0)
    // 28 sondages de plus mènent à 12:02:35 : 12:01:00 et 12:02:00 tombent
    // dans la fenêtre, 12:00:00 est avant son début.
    await vi.advanceTimersByTimeAsync(28 * 5000)
    await flushPromises()
    expect(w.findAll('[data-system-history-tick]').length).toBe(2)
    w.unmount()
  })

  it('changer la période ne sonde step sur-le-champ tant que l échéance court encore', async () => {
    // 5 s par défaut, on avance de 1 s, puis on passe à 10 s : le last
    // sondage a 1 s, l'échéance neuve est à 10 s, donc rien ne doit partir
    // avant les 9 s remaining.
    const f = stub(payload())
    const w = await monter()
    await vi.advanceTimersByTimeAsync(1000)
    const avant = f.mock.calls.length
    await w.findComponent(Select).vm.$emit('update:modelValue', '10')
    await flushPromises()
    expect(f.mock.calls.length).toBe(avant)
    await vi.advanceTimersByTimeAsync(8000)
    expect(f.mock.calls.length).toBe(avant)
    await vi.advanceTimersByTimeAsync(1500)
    expect(f.mock.calls.length).toBe(avant + 1)
    w.unmount()
  })

  it('changer la période sonde tout de suite si elle rend le last sondage périmé', async () => {
    // 5 s par défaut, on avance de 4 s, puis on passe à 1 s : le last
    // sondage a 4 s pour une période de 1 s, il est donc déjà périmé et la
    // reprise doit être immédiate — sans quoi la page resterait sur des
    // chiffres vieux de plusieurs périodes après avoir demandé d'accélérer.
    const f = stub(payload())
    const w = await monter()
    await vi.advanceTimersByTimeAsync(4000)
    const avant = f.mock.calls.length
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    expect(f.mock.calls.length).toBe(avant + 1)
    w.unmount()
  })

  it('un changement de période pendant un sondage en vol n écrase step un état plus frais', async () => {
    type Differe = { signal: AbortSignal | null | undefined; resolve: (v: unknown) => void }
    const differes: Differe[] = []
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      if (String(url).includes('/api/i18n')) return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      // Même raison que dans le test du verrou : le log est relevé une seule
      // fois au montage, hors du sondage, et n'a rien à faire dans `differes`.
      // Les réglages non plus, relevés une fois pour dater ce log.
      if (String(url).includes('/api/logs')) return Promise.resolve({ ok: true, json: async () => ({ lines: [] }) } as Response)
      if (String(url).includes('/api/settings')) {
        return Promise.resolve({ ok: true, json: async () => ({ date_format: 'day_month_year', clock_24h: true }) } as Response)
      }
      return new Promise((resolve, reject) => {
        const signal = init?.signal
        // Un `AbortSignal` réel rejette son `fetch` à l'annulation : le stub
        // reproduit ce comportement plutôt que de laisser la promesse en
        // vol pour toujours.
        signal?.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')))
        differes.push({ signal, resolve })
      })
    })
    vi.stubGlobal('fetch', f)
    const w = await monter()
    expect(differes.length).toBe(1)
    // Changement de période pendant que ce premier sondage est encore en vol.
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    // `stop()` a dû annuler la requête en vol...
    expect(differes[0]!.signal?.aborted).toBe(true)
    // ... sans que cette annulation count pour une panne : une requête
    // abandonnée par notre propre code n'est step un échec du cœur.
    expect(w.find('[data-system-unavailable]').exists()).toBe(false)
    // ... et la reprise n'est plus immédiate : le last sondage venant tout
    // juste d'être lancé, l'échéance du nouveau rythme (1 s) n'est step
    // atteinte. C'est elle qui relancera.
    expect(differes.length).toBe(1)
    await vi.advanceTimersByTimeAsync(1000)
    expect(differes.length).toBe(2)
    // La requête annulée finit par « répondre » avec des données pourtant
    // plus anciennes que celles déjà posées par la requête plus fraîche :
    // elle ne doit ni les écraser, ni afficher la ligne d'indisponibilité —
    // une requête abandonnée par notre propre code n'est step un échec du
    // cœur.
    differes[1]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 1000 }) })
    await flushPromises()
    expect(w.find('[data-system-unavailable]').exists()).toBe(false)
    w.unmount()
  })

  it('re-choose la période déjà active ne redéclenche step le sondage', async () => {
    const f = stub(payload())
    const w = await monter()
    await flushPromises()
    const appels = f.mock.calls.length
    // La valeur initiale du sélecteur est déjà « 5 » (période par défaut) :
    // la re-choose ne doit ni probe immédiatement, ni réinitialiser le
    // timer.
    await w.findComponent(Select).vm.$emit('update:modelValue', '5')
    await flushPromises()
    expect(f.mock.calls.length).toBe(appels)
    w.unmount()
  })

  it('change la cadence de sondage en changeant la période', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.findComponent(Select).vm.$emit('update:modelValue', '1')
    await flushPromises()
    const appels = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(3000)
    await flushPromises()
    // À une seconde, trois sondages supplémentaires en 3 s ; la période
    // précédente de 5 s n'en aurait produit aucun sur la même durée.
    expect(f.mock.calls.length - appels).toBe(3)
    w.unmount()
  })

  it('ordonne les cartes CPU, Mémoire, Historique, Stockage, Appareil, Alimentation', async () => {
    stub(payload())
    const w = await monter()
    const titres = w.findAllComponents(CardTitle).map((c) => c.text())
    expect(titres[0]).toBe('system_cpu')
    expect(titres[1]).toBe('system_memory')
    expect(titres[2]).toContain('system_history')
    expect(titres[3]).toBe('system_storage')
    expect(titres[4]).toBe('system_device')
    expect(titres[5]).toBe('system_power')
    w.unmount()
  })

  it('displayed la load moyenne dans la carte history', async () => {
    stub(payload())
    const w = await monter()
    expect(w.get('[data-system-load]').text()).toBe('0.50 · 0.40 · 0.30')
    w.unmount()
  })

  it('displayed un tiret pour la voltage quand aucune sonde n est presente', async () => {
    // `null` : step de capteur `rpi_volt`, à distinguer d'une alimentation
    // saine (`false`) — l'ancien affichage confondait les deux.
    stub(payload({ under_voltage: null }))
    const w = await monter()
    const voltage = w.get('[data-system-under-voltage]')
    expect(voltage.text()).toBe('—')
    expect(voltage.classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('displayed la voltage nominale quand la sonde ne detecte rien', async () => {
    stub(payload({ under_voltage: false }))
    const w = await monter()
    const voltage = w.get('[data-system-under-voltage]')
    expect(voltage.text()).toBe('system_voltage_ok')
    expect(voltage.classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('displayed l antecedent quand la sonde est saine mais un episode a eu lieu depuis le demarrage', async () => {
    // `under_voltage: false` (rien à l'instant) mais `under_voltage_since_boot:
    // true` (le bit collant du micrologiciel) : un troisième état, distinct
    // de la sous-voltage en cours, sans le rouge de l'alerte immédiate.
    stub(payload({ under_voltage: false, under_voltage_since_boot: true }))
    const w = await monter()
    const voltage = w.get('[data-system-under-voltage]')
    expect(voltage.text()).toBe('system_voltage_since_boot')
    expect(voltage.classes()).not.toContain('text-destructive')
    // La phrase de conseil reste réservée à l'alerte instantanée.
    expect(w.find('[data-system-under-voltage-avis]').exists()).toBe(false)
    w.unmount()
  })

  it('l alerte instantanee l emporte sur l antecedent quand les deux sont vrais', async () => {
    stub(payload({ under_voltage: true, under_voltage_since_boot: true }))
    const w = await monter()
    expect(w.get('[data-system-under-voltage]').text()).toBe('system_voltage_low')
    w.unmount()
  })

  it('displayed l alerte en rouge en cas de sous-voltage detectee', async () => {
    stub(payload({ under_voltage: true }))
    const w = await monter()
    const voltage = w.get('[data-system-under-voltage]')
    // Le mot court dans la grille, step la phrase entière : voir le test
    // suivant pour la phrase de conseil, affichée à part.
    expect(voltage.text()).toBe('system_voltage_low')
    expect(voltage.classes()).toContain('text-destructive')
    w.unmount()
  })

  it('displayed la phrase de conseil sous la grille seulement quand l alerte est active', async () => {
    stub(payload({ under_voltage: false }))
    const w = await monter()
    expect(w.find('[data-system-under-voltage-avis]').exists()).toBe(false)
    w.unmount()
  })

  it('displayed la phrase de conseil avec role status en cas de sous-voltage', async () => {
    stub(payload({ under_voltage: true }))
    const w = await monter()
    const avis = w.get('[data-system-under-voltage-avis]')
    expect(avis.text()).toBe('system_under_voltage')
    expect(avis.attributes('role')).toBe('status')
    w.unmount()
  })

  it('le bouton d aide sur la voltage porte un nom accessible et ouvre la popin', async () => {
    stub(payload())
    const w = await monter()
    const bouton = w.get('[data-system-voltage-help]')
    expect(bouton.attributes('aria-label')).toBe('system_voltage_help')
    // Fermée au départ : la popin ne doit step s'imposer à l'arrivée sur la page.
    expect(document.body.querySelector('[role="dialog"]')).toBeNull()
    await bouton.trigger('click')
    await flushPromises()
    // Montée dans un portail, comme le dialog d'alimentation.
    expect(document.body.textContent).toContain('system_voltage_help_title')
    expect(document.body.textContent).toContain('system_voltage_help_body')
    w.unmount()
  })

  it('l étiquette de période se corrige quand le catalogue arrive après le montage', async () => {
    // Reproduit l'order réel d'un premier chargement : `App.vue` lance le
    // rechargement du catalogue à SON montage, donc la vue se mounted avant que
    // la réponse arrive. Tous les libellés se corrigent ensuite d'eux-mêmes,
    // `t` étant une computed — sauf celui du déclencheur du Select, que
    // `SelectValue` sans contenu figeait sur le text capturé au montage : la
    // list affichait « 5 system_unit_second » pour toujours.
    //
    // Ce test échouerait donc en rendant `<SelectValue />` sans contenu.
    // `monter()` ne peut step le voir : il load le catalogue AVANT de monter.
    stub(payload(), {})
    await useCatalog().reload()
    // `App.vue` amorce le sondage au montage de la SPA, plus la vue : le
    // harness de test tient ce rôle, dans le même order que l'application.
    useMetrics().start()
    const w = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    expect(w.get('[data-system-period]').text()).toContain('system_unit_second')

    stub(payload(), { ...CATALOGUE, system_unit_second: 's' })
    await useCatalog().reload()
    await flushPromises()
    expect(w.get('[data-system-period]').text()).toBe('5 s')
    w.unmount()
  })

  it('le libellé de la fenêtre suit la période choisie', async () => {
    stub(payload())
    const w = await monter()
    expect(w.get('[data-system-history-span]').text()).toBe('20 min')
    await w.findComponent(Select).vm.$emit('update:modelValue', '30')
    await flushPromises()
    expect(w.get('[data-system-history-span]').text()).toBe('120 min')
    w.unmount()
  })

  it('displayed la fenêtre de repli (capacité × période) tant que l history ne mesure rien', async () => {
    // Page fraîche : aucun échantillon encore poussé (seul le premier sondage
    // de référence a eu lieu), donc rien à mesurer — repli sur la capacité
    // théorique à la période par défaut (5 s × 240 = 20 min).
    stub(payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 }))
    const w = await monter()
    expect(w.get('[data-system-history-span]').text()).toBe('20 min')
    w.unmount()
  })

  it('displayed la durée réelle de l history plutôt que la capacité une fois mesurable', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Trois sondages supplémentaires à 5 s : le premier ne fait que poser la
    // référence de jiffies, les deux suivants poussent deux échantillons
    // distants de 5 s réels — bien moins que les 20 min que promettrait la
    // capacité théorique à cette période.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(w.get('[data-system-history-span]').text()).toBe('0 min')
    w.unmount()
  })

  describe('survol de l history', () => {
    /**
     * Cinq réponses successives aux valeurs bien séparées (cpu 10/30/50/70/90 %,
     * ram 5/25/45/65/85 %) : de quoi distinguer sans ambiguïté la colonne
     * pointée. La toute première réponse ne fait que poser la référence de
     * jiffies (voir `cpuUsage`) et ne pousse aucun échantillon.
     */
    function reponsesSurvol() {
      const cibles: [cpu: number, ram: number][] = [
        [10, 5],
        [30, 25],
        [50, 45],
        [70, 65],
        [90, 85],
      ]
      const reponses = [payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 })]
      let total = 0
      let idle = 0
      for (const [cpuCible, ramCible] of cibles) {
        total += 1000
        idle += 1000 * (1 - cpuCible / 100)
        reponses.push(
          payload({
            cpu_total_jiffies: total,
            cpu_idle_jiffies: idle,
            memory: { total_kb: 1_000_000, available_kb: 1_000_000 * (1 - ramCible / 100) },
          }),
        )
      }
      return reponses
    }

    /**
     * Monte la vue avec les cinq échantillons ci-dessus déjà en history, et
     * stube le rectangle du graphe : sous jsdom, `getBoundingClientRect`
     * renvoie des zéros, et tout x se ramènerait au même index sans ce stub.
     */
    async function monterAvecHistorique() {
      const reponses = reponsesSurvol()
      let i = 0
      stub(() => reponses[Math.min(i++, reponses.length - 1)])
      const w = await monter()
      await vi.advanceTimersByTimeAsync(5 * 5000)
      await flushPromises()
      const svg = w.get('[data-system-history]')
      vi.spyOn(svg.element, 'getBoundingClientRect').mockReturnValue({
        left: 0, width: 200, top: 0, height: 0, right: 200, bottom: 0, x: 0, y: 0, toJSON: () => {},
      } as DOMRect)
      return { w, svg }
    }

    it('un pointeur au milieu du graphe displayed l échantillon du milieu', async () => {
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 100 })
      const popin = w.get('[data-system-history-popin]')
      expect(popin.text()).toContain('50 %')
      expect(popin.text()).toContain('45 %')
      w.unmount()
    })

    it('un pointeur sur la première colonne displayed le premier échantillon', async () => {
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 0 })
      const popin = w.get('[data-system-history-popin]')
      expect(popin.text()).toContain('10 %')
      expect(popin.text()).toContain('5 %')
      w.unmount()
    })

    it('un pointeur sur la dernière colonne displayed le last échantillon', async () => {
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 200 })
      const popin = w.get('[data-system-history-popin]')
      expect(popin.text()).toContain('90 %')
      expect(popin.text()).toContain('85 %')
      w.unmount()
    })

    it('le popin apparaît au survol et disparaît en quittant le graphe', async () => {
      const { w, svg } = await monterAvecHistorique()
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      await svg.trigger('pointermove', { clientX: 100 })
      expect(w.find('[data-system-history-popin]').exists()).toBe(true)
      await svg.trigger('pointerleave')
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      w.unmount()
    })

    it('un appui tactile displayed le popin sans attendre un mouvement', async () => {
      // `pointerdown` seul, sans `pointermove` : un tap immobile sur écran
      // tactile ne déclencherait jamais `pointermove`.
      const { w, svg } = await monterAvecHistorique()
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      await svg.trigger('pointerdown', { clientX: 100 })
      const popin = w.get('[data-system-history-popin]')
      expect(popin.text()).toContain('50 %')
      w.unmount()
    })

    it('un geste interrompu efface le popin', async () => {
      // `pointercancel` : le geste est interrompu (un défilement de page qui
      // démarre, par exemple) sans qu'un `pointerup` n'ait jamais eu lieu.
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointerdown', { clientX: 100 })
      expect(w.find('[data-system-history-popin]').exists()).toBe(true)
      await svg.trigger('pointercancel')
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      w.unmount()
    })

    it('le trait de survol suit la colonne pointée', async () => {
      // `WIDTH` du viewBox vaut 100, n = 5 : le step entre colonnes vaut
      // 25. La colonne 2 (survolée par `clientX: 100`, voir le test du
      // milieu ci-dessus) doit donc placer le trait à x = 50.
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 100 })
      const ligne = w.get('[data-system-history-line]')
      expect(ligne.attributes('x1')).toBe('50')
      expect(ligne.attributes('x2')).toBe('50')
      w.unmount()
    })

    it('arrondit à la colonne la plus proche plutôt que d arrondir vers le bas', async () => {
      // n = 5 sur une largeur de 200 px : les colonnes tombent à 0, 50, 100,
      // 150, 200. `clientX: 95` (fraction 1,9) et `clientX: 105` (fraction
      // 2,1) doivent tous deux désigner la colonne 2 : un `Math.floor`
      // donnerait 1 pour le premier et 2 pour le second, deux réponses
      // différentes là où « la plus proche » n'en admet qu'une.
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 95 })
      let ligne = w.get('[data-system-history-line]')
      expect(ligne.attributes('x1')).toBe('50')
      await svg.trigger('pointermove', { clientX: 105 })
      ligne = w.get('[data-system-history-line]')
      expect(ligne.attributes('x1')).toBe('50')
      // `clientX: 125` (fraction 2,5, à cheval entre les colonnes 2 et 3) :
      // `Math.round` arrondit les demis vers le haut, donc la colonne 3
      // (x = 75), ce qu'un arrondi « au plus proche » différent (vers le
      // pair, par exemple) ne donnerait step forcément.
      await svg.trigger('pointermove', { clientX: 125 })
      ligne = w.get('[data-system-history-line]')
      expect(ligne.attributes('x1')).toBe('75')
      w.unmount()
    })

    it('le popin est centré par une transformation constante, bornée en pixels sur les trois régimes', async () => {
      // Graphe large de 200 px (voir le stub de `getBoundingClientRect`
      // ci-dessus), popin large de 100 px (`POPOVER_WIDTH_PX`) : le centre
      // idéal ne peut descendre sous 50 px ni dépasser 150 px sans faire
      // déborder le popin de la carte.
      const { w, svg } = await monterAvecHistorique()
      // Première colonne (i = 0 sur 5) : centre idéal à 0 px, borné à 50 px —
      // la transformation reste -50 % constante, c'est la position qui est
      // bornée, step un cas particulier de transformation comme avant cette
      // série.
      await svg.trigger('pointermove', { clientX: 0 })
      let popin = w.get('[data-system-history-popin]').element as HTMLElement
      expect(popin.style.transform).toBe('translateX(-50%)')
      expect(popin.style.left).toBe('50px')
      // Colonne du milieu (i = 2 sur 5) : centre idéal à 100 px, dans la
      // bande non bornée — c'était la branche non testée avant cette série,
      // celle où l'ancien code centrait sans jamais borner.
      await svg.trigger('pointermove', { clientX: 100 })
      popin = w.get('[data-system-history-popin]').element as HTMLElement
      expect(popin.style.transform).toBe('translateX(-50%)')
      expect(popin.style.left).toBe('100px')
      // Dernière colonne (i = 4 sur 5) : centre idéal à 200 px, borné à
      // 150 px, symétrique de la première colonne.
      await svg.trigger('pointermove', { clientX: 200 })
      popin = w.get('[data-system-history-popin]').element as HTMLElement
      expect(popin.style.transform).toBe('translateX(-50%)')
      expect(popin.style.left).toBe('150px')
      w.unmount()
    })

    it('survoler un graphe encore vide n displayed ni popin ni trait', async () => {
      // Un seul sondage : la référence de jiffies est posée, aucun échantillon
      // poussé. Le graphe est là quand même (il l'est désormais toujours, pour
      // que la mise en page ne saute step), donc il est **survolable** avant
      // d'avoir la moindre donnée — ce que l'ancienne version rendait
      // impossible en ne le dessinant step. Le garde `< 2` de `hoverPointer`
      // et celui de `hoverLineX` deviennent donc porteurs : ce test les
      // épingle.
      stub(payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 }))
      const w = await monter()
      const svg = w.get('[data-system-history]')
      await svg.trigger('pointermove', { clientX: 100 })
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      expect(w.find('[data-system-history-line]').exists()).toBe(false)
      w.unmount()
    })
  })

  describe('courbe de température', () => {
    it('trace la température comme troisième courbe', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      const d = w.get('[data-system-history-temp]').attributes('d')!
      expect(d).not.toBe('')
      // Même échelle que les pourcentages : 47,8 °C se lit à mi-hauteur d'un
      // repère de 30, donc autour de y = 15.
      expect(d).toMatch(/^M[\d.]+,1[0-9]\.\d\d/)
      w.unmount()
    })

    it('ne trace rien sans sonde de température', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: null }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      // Les deux autres courbes restent : une machine sans sonde ne perd step
      // son graphe.
      expect(w.get('[data-system-history] path').attributes('d')).not.toBe('')
      expect(w.get('[data-system-history-temp]').attributes('d')).toBe('')
      w.unmount()
    })

    it('un trou passager dans la série creuse la courbe sans l effacer', async () => {
      // Une lecture manquante n'efface plus la courbe entière : chaque
      // température présente reste sur sa propre abscisse (son horodatage),
      // exactement comme dans les deux autres courbes, donc rien ne dérive
      // même quand la série a un trou au milieu. `sparklinePath` referme le
      // sous-tracé courant sur le `null` et rouvre un `M` au prochain point
      // présent : deux sous-tracés SVG plutôt qu'un tracé absent.
      const jiffies = prochainsJiffies()
      let tour = 0
      stub(() => payload({ ...jiffies(), temperature_c: tour++ === 2 ? null : 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(20000)
      await flushPromises()
      const d = w.get('[data-system-history-temp]').attributes('d')!
      expect(d).not.toBe('')
      // Deux sous-tracés : celui d'avant le trou, celui d'après.
      expect((d.match(/M/g) ?? []).length).toBe(2)
      w.unmount()
    })

    it('annonce la température dans la légende', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      expect(w.get('[data-system-history-legend]').text()).toContain('47.8 °C')
      w.unmount()
    })

    it('n annonce step de température dans la légende sans sonde', async () => {
      stub(payload({ temperature_c: null }))
      const w = await monter()
      // Pas de série annoncée quand aucune courbe ne peut exister : l'absence
      // de sonde est connue dès le premier sondage, donc rien ne saute.
      expect(w.get('[data-system-history-legend]').text()).not.toContain('°C')
      w.unmount()
    })

    it('displayed la température dans le popin de survol', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      const svg = w.get('[data-system-history]')
      vi.spyOn(svg.element, 'getBoundingClientRect').mockReturnValue({
        left: 0, width: 200, top: 0, height: 0, right: 200, bottom: 0, x: 0, y: 0, toJSON: () => {},
      } as DOMRect)
      await svg.trigger('pointermove', { clientX: 10 })
      await flushPromises()
      expect(w.get('[data-system-history-popin]').text()).toContain('47.8 °C')
      w.unmount()
    })
  })

  describe('dernières erreurs', () => {
    it('rend une ligne par entrée de log, dans l’order reçu', async () => {
      // `/api/logs` rend déjà les plus récentes en premier (le cœur inverse son
      // tampon), la vue ne retrie step : elle doit rendre l'order tel quel.
      stub(payload(), CATALOGUE, {
        lines: ['WARN la plus recente', 'WARN la plus ancienne'],
      })
      const w = await monter()
      expect(w.findAll('[data-log-line]').map((l) => l.text())).toEqual([
        'WARN la plus recente',
        'WARN la plus ancienne',
      ])
      w.unmount()
    })

    it('aucune erreur récente : aucune ligne, et la carte reste rendue', async () => {
      stub(payload(), { ...CATALOGUE, recent_errors: 'Dernières erreurs' })
      const w = await monter()
      expect(w.findAll('[data-log-line]')).toHaveLength(0)
      expect(w.text()).toContain('Dernières erreurs')
      w.unmount()
    })

    it('un log injoignable ne prive step la page de ses métriques', async () => {
      // Les deux relevés sont indépendants, chacun avec son `.catch` : un
      // `/api/logs` en panne ne doit step faire passer la machine pour muette —
      // ce sont justement les métriques qu'on regarde quand le log manque.
      stub(payload(), CATALOGUE, undefined)
      const w = await monter()
      expect(w.findAll('[data-log-line]')).toHaveLength(0)
      expect(w.find('[data-system-unavailable]').exists()).toBe(false)
      expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
      w.unmount()
    })

    it('le sondage périodique ne relève step le log', async () => {
      // Greffer le log sur `probe()` allongerait la prise du verrou « en
      // vol » et changerait la cadence observée : mesuré, quatre tests de
      // cadence tombaient. Ce test épingle la séparation.
      const f = stub(payload(), CATALOGUE, { lines: ['WARN une erreur'] })
      const w = await monter()
      const auMontage = f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length
      expect(auMontage).toBe(1)
      await vi.advanceTimersByTimeAsync(20000)
      await flushPromises()
      expect(f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length).toBe(auMontage)
      // Et les métriques, elles, ont bien continué d'être sondées.
      expect(
        f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length,
      ).toBeGreaterThan(1)
      w.unmount()
    })
  })

  describe('popin des erreurs', () => {
    /** Douze lignes : plus que les huit de la carte, assez pour que le filtre
     *  ait quelque chose à écarter. */
    const DOUZE = Array.from({ length: 12 }, (_, i) =>
      i === 3 ? 'ERROR mpv socket closed' : `WARN ligne ${i}`,
    )

    it('la carte ne montre que les huit erreurs les plus récentes', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      expect(w.findAll('[data-log-line]')).toHaveLength(8)
      expect(w.findAll('[data-log-line]')[0]!.text()).toBe(DOUZE[0])
      w.unmount()
    })

    it('le bouton annonce le total et s offre dès la première erreur', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      expect(w.get('[data-logs-all]').text()).toContain('12')
      w.unmount()

      // Trois erreurs : la carte les montre déjà toutes, et le bouton reste
      // offert quand même. Signalé à l'usage — reserve au log long, le
      // filtre ne se decouvrait qu'au pire moment, quand il y a trop a lire
      // pour explorer l'ecran.
      stub(payload(), CATALOGUE, { lines: DOUZE.slice(0, 3) })
      const peu = await monter()
      expect(peu.get('[data-logs-all]').text()).toContain('3')
      peu.unmount()

      // Journal vide : il n'y a rien a ouvrir, le bouton disparait.
      stub(payload(), CATALOGUE, { lines: [] })
      const vide = await monter()
      expect(vide.find('[data-logs-all]').exists()).toBe(false)
      vide.unmount()
    })

    it('la popin list tout le log', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // La popin est rendue dans un portail : elle vit dans document.body.
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('12 / 12')
      w.unmount()
    })

    it('le champ filtre la list et met à jour le compteur', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const champ = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      champ.value = 'mpv'
      champ.dispatchEvent(new Event('input'))
      await flushPromises()
      const lignes = document.body.querySelectorAll('[data-logs-dialog-line]')
      expect(lignes).toHaveLength(1)
      expect(lignes[0]!.textContent).toBe('ERROR mpv socket closed')
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('1 / 12')
      w.unmount()
    })

    it('annonce l absence de correspondance', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const champ = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      champ.value = 'zzz'
      champ.dispatchEvent(new Event('input'))
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(0)
      expect(document.body.querySelector('[data-logs-empty]')).not.toBeNull()
      w.unmount()
    })

    it('relève le log à l ouverture', async () => {
      const f = stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      const avant = f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // Une requête de plus, sur geste utilisateur : le log reste hors du
      // sondage périodique (verrou « en vol » et delta CPU de `probe`).
      expect(f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length).toBe(avant + 1)
      w.unmount()
    })

    it('rouvre sans le filtre précédent', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const champ = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      champ.value = 'mpv'
      champ.dispatchEvent(new Event('input'))
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(1)

      // Fermeture par le bouton du dialog — le vrai geste, et le seul
      // `[data-slot="dialog-close"]` présent puisque seul le dialog open
      // est rendu dans le portail. Puis réouverture : le champ repart vide,
      // sinon la popin s'ouvrirait sur une list tronquée sans que rien à
      // l'écran ne l'explique.
      document.body.querySelector<HTMLElement>('[data-slot="dialog-close"]')!.click()
      await flushPromises()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      w.unmount()
    })
  })
})
