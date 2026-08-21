import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { CardTitle, Select } from '@ritornello/ui'
import { useCatalog } from '../composables/useCatalog'
import SystemView from './SystemView.vue'

// Charge utile complète, réutilisée en la modifiant par cas. Les jiffies CPU
// valent `null` par défaut : c'est le cas « la machine ne les expose pas »,
// pas une panne — les tests qui ont besoin d'un delta calculable les
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
 * d'historique, qui ont besoin d'un delta réel plutôt que de valeurs figées.
 */
function prochainsJiffies() {
  let n = 0
  return () => {
    n += 1
    return { cpu_total_jiffies: n * 1000, cpu_idle_jiffies: n * 750 }
  }
}

/** Catalogue minimal : les unités, le gabarit de la fenêtre d'historique et
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
 * `/api/system` de l'autre, `{}` pour les POST. `corps` accepte une fonction,
 * appelée à chaque sondage, pour faire varier les réponses successives.
 *
 * Le catalogue est bel et bien servi : sans lui, `createT` renvoie la clé
 * elle-même et les unités s'afficheraient « system_unit_day ». Le test
 * vérifierait alors le repli, pas la vue.
 */
function stub(
  corps: unknown | (() => unknown),
  catalogue: Record<string, string> = CATALOGUE,
  journal: unknown = { lines: [] },
) {
  const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
    }
    const u = String(url)
    // `/api/logs` distingué de `/api/system` : la page sonde les deux à chaque
    // tour, et servir la charge des métriques au journal lui donnerait un
    // `lines` absent — le test échouerait alors pour une raison qui n'est pas
    // la sienne.
    if (u.includes('/api/logs')) {
      if (journal === undefined) {
        return Promise.resolve({ ok: false, status: 503, json: async () => ({}) } as Response)
      }
      return Promise.resolve({ ok: true, json: async () => journal } as Response)
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
  beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }))
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    // Les dialogues sont montés dans un portail : sans ce nettoyage, le DOM
    // d'un test fuiterait dans les `document.body.querySelector` du suivant.
    document.body.innerHTML = ''
  })

  /**
   * Charge le catalogue puis monte la vue — c'est l'ordre de l'application,
   * `App.vue` rechargeant le catalogue au montage. `attachTo` est nécessaire
   * aux tests du dialogue et inoffensif pour les autres.
   */
  async function monter() {
    await useCatalog().reload()
    const w = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    return w
  }

  it('affiche les métriques du premier sondage', async () => {
    stub(payload())
    const w = await monter()
    expect(w.get('[data-system-temperature]').text()).toContain('47.8')
    expect(w.get('[data-system-frequency]').text()).toContain('900')
    expect(w.get('[data-system-cores]').text()).toBe('4')
    expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
    expect(w.get('[data-system-kernel]').text()).toBe('6.6.51+rpt-rpi-v7')
    // 90 061 s = 1 jour 1 heure, au plus deux unités.
    expect(w.get('[data-system-uptime]').text()).toBe('1 j 1 h')
    // 600 000 kio utilisés sur 1 000 000, arrondis en Mo.
    expect(w.get('[data-system-memory]').text()).toBe('586 / 977 Mo')
    w.unmount()
  })

  it('affiche un tiret pour ce que la machine n expose pas', async () => {
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

  it('arrive avec un historique vide et le remplit au fil des sondages', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Un seul échantillon : le graphe est **déjà là** mais ne trace rien, et
    // sa légende annonce « — » plutôt que « 0 % ». C'est ce qui évite le saut
    // de mise en page au deuxième sondage, quand un message d'attente cédait
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

  it('plafonne l historique à 60 échantillons', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Le montage ne pousse aucun échantillon : il faut un delta de jiffies,
    // donc un premier sondage de référence avant que quoi que ce soit ne
    // soit calculable. 61 sondages supplémentaires (période de 5 s) poussent
    // donc 61 échantillons, le 61e faisant sortir le plus ancien par
    // `shift()` : il doit en rester exactement 60, soit 59 commandes « L »
    // dans le tracé (un « M » puis n-1 « L »).
    await vi.advanceTimersByTimeAsync(61 * 5000)
    await flushPromises()
    // Le tracé est porté par le premier `<path>`, `[data-system-history]`
    // marquant le `<svg>` qui les contient tous les deux.
    const d = w.get('[data-system-history] path').attributes('d')!
    expect((d.match(/L/g) ?? []).length).toBe(59)
    w.unmount()
  })

  it('espace les points selon le temps réel quand la période change en cours de route', async () => {
    // Le comportement demandé : après un passage de 5 s à 1 s, les
    // échantillons anciens restent largement espacés et les récents se
    // resserrent. Un placement par rang les aurait rendus tous égaux, faisant
    // passer 5 s d'histoire pour 1 s. Ce test couvre le **câblage** (les deux
    // tracés, le trait de survol et le popin partagent `abscissesGraphe`) ;
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
    const dernier = ecarts.at(-1) ?? 0
    // Rapport théorique 5 (5 s contre 1 s) ; on exige 3 pour laisser passer le
    // temps réel qui s'écoule aussi sous `shouldAdvanceTime`.
    expect(premier).toBeGreaterThan(dernier * 3)
    w.unmount()
  })

  it('arrête de sonder au démontage', async () => {
    const f = stub(payload())
    const w = await monter()
    const appels = f.mock.calls.length
    w.unmount()
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBe(appels)
  })

  it('désactive les boutons système quand polkit n est pas configuré', async () => {
    stub(payload({ can_power_off: false, can_reboot: false }))
    const w = await monter()
    expect(w.get('[data-power-poweroff]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-power-reboot]').attributes('disabled')).toBeDefined()
    // Le redémarrage du service ne dépend d'aucune autorisation.
    expect(w.get('[data-power-restart]').attributes('disabled')).toBeUndefined()
    expect(w.find('[data-power-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('accuse polkit quand logind a repondu, logind quand il n a pas repondu', async () => {
    // Les deux causes d'un bouton grisé ne se réparent pas pareil, et la
    // phrase qui les confond envoie chercher une règle polkit déjà en place.
    // Le catalogue de test ne porte pas ces clés : `t` rend la clé, ce qui
    // suffit à distinguer les deux phrases.
    stub(payload({ can_power_off: false, can_reboot: false, logind_reachable: true }))
    const refus = await monter()
    expect(refus.get('[data-power-unavailable]').text()).toContain('system_power_unavailable')
    refus.unmount()

    stub(payload({ can_power_off: false, can_reboot: false, logind_reachable: false }))
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
    // Le dialogue est monté dans un portail : il vit dans document.body.
    expect(document.body.querySelector('[data-power-confirm]')).not.toBeNull()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    document.body.querySelector<HTMLElement>('[data-power-cancel]')!.click()
    await flushPromises()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    w.unmount()
  })

  it('poste l action confirmée puis annonce l arrêt et cesse de sonder', async () => {
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

  it('ne relance pas le sondage sur un retour de visibilité pendant un arrêt confirmé', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    const appels = f.mock.calls.length
    // L'utilisateur change d'onglet puis revient : `visibilitychange` doit
    // rappeler `demarrer()`, qui ne doit rien faire tant que l'arrêt est en
    // cours, sans quoi la page sonderait un cœur déjà parti.
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(5000)
    expect(f.mock.calls.length).toBe(appels)
    w.unmount()
  })

  it('reprend la main quand le redémarrage du service aboutit', async () => {
    // Uptime décroissant : le service est bien revenu, ce qu'une simple
    // réponse ne prouverait pas (le premier sondage peut encore atteindre
    // l'ancien process). Les deux premières réponses portent un uptime
    // largement supérieur à `avant + écoulé` (3600 + quelques secondes tout
    // au plus dans ce test) : sans cette marge, elles satisferaient déjà le
    // nouveau seuil et l'attente s'arrêterait dès le premier sondage au lieu
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
    // Après le POST, chaque GET reste en attente pour toujours : une
    // requête qui se connecte mais ne répond jamais. Sans la course contre
    // un délai dans `attendreRetour`, l'attente resterait bloquée dessus au
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

  it('démonter pendant l attente ne relance pas le sondage ensuite', async () => {
    // L'ancien processus répond encore, donc son uptime **croît** avec
    // l'horloge — c'est ce que fait un processus toujours vivant. La condition
    // de retour le compare à `avant + écoulé` : un uptime qui suit l'horloge
    // ne peut jamais passer sous ce seuil, donc l'attente tourne jusqu'à ce
    // qu'on démonte la vue. Un échantillon figé, lui, finirait par être pris
    // pour un redémarrage réussi et ce test ne dirait plus ce qu'il annonce.
    const debut = Date.now()
    const f = stub(() => payload({ service_uptime_s: 3600 + Math.floor((Date.now() - debut) / 1000) }))
    const w = await monter()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    w.unmount()
    // Une requête déjà en vol au moment du démontage est normale (la boucle
    // ne s'arrête qu'au tour suivant) ; ce qui ne doit jamais se produire,
    // c'est un nouveau minuteur créé après coup par `demarrer()`.
    await vi.advanceTimersByTimeAsync(10000)
    const appels = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(30000)
    expect(f.mock.calls.length).toBe(appels)
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

  it('affiche un tiret pour l utilisation CPU au premier sondage', async () => {
    // Pas de sondage précédent : aucun delta n'est calculable, et ce n'est
    // pas une panne.
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await monter()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    w.unmount()
  })

  it('affiche un tiret quand le delta total est nul ou négatif', async () => {
    // Mêmes compteurs à chaque sondage : Δtotal = 0.
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    w.unmount()
  })

  /** Deux sondages dont le delta donne le pourcentage voulu. */
  function jiffiesPour(pourcent: number) {
    const idle = 1000 - pourcent * 10
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
    // En dessous du seuil : couleur normale, pour les deux éléments.
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

  it('90 pour cent pile n est pas encore une alerte', async () => {
    // Le seuil est strict : sans cela une charge nominale afficherait du rouge.
    stub(jiffiesPour(90))
    const w = await monter()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('90 %')
    expect(w.get('[data-system-cpu-usage]').classes()).not.toContain('text-destructive')
    expect(w.get('[data-system-cpu-bar] div').classes()).toContain('bg-primary')
    w.unmount()
  })

  it('un pourcentage entre 90 et 90 virgule 5 affiche 90 pour cent sans alerte', async () => {
    // Le libellé affiché est arrondi (`Math.round`) : le seuil doit comparer
    // cette même valeur arrondie, pas la valeur brute — sinon 90 < u <= 90,5
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

  it('affiche la barre CPU à zéro tant que le pourcentage est inconnu', async () => {
    // La barre est là dès le premier rendu, vide : elle apparaissait sinon
    // d'un coup au deuxième sondage en poussant la mise en page. Rien ne
    // prétend « 0 % » pour autant — la ligne de lecture affiche « — ».
    stub(payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }))
    const w = await monter()
    expect(w.get('[data-system-cpu-usage]').text()).toBe('—')
    expect(w.get('[data-system-cpu-bar] div').attributes('style')).toContain('width: 0%')
    w.unmount()
  })

  it('un sondage en vol empêche un second sondage de corrompre le delta suivant', async () => {
    // Chaque GET reste en attente jusqu'à ce que le test le résolve
    // explicitement, pour simuler un sondage qui n'a pas encore répondu
    // quand le minuteur tique à nouveau.
    const differes: { resolve: (v: unknown) => void }[] = []
    let n = 0
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      if (String(url).includes('/api/i18n')) return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      // Le journal est relevé une fois au montage et ne passe pas par le verrou
      // de sondage : le compter ici mesurerait autre chose que ce test.
      if (String(url).includes('/api/logs')) return Promise.resolve({ ok: true, json: async () => ({ lines: [] }) } as Response)
      n += 1
      return new Promise((resolve) => differes.push({ resolve }))
    })
    vi.stubGlobal('fetch', f)
    const w = await monter()
    // Premier sondage (déclenché par `demarrer()` au montage) : en vol.
    expect(n).toBe(1)
    // Le minuteur tique pendant que ce premier sondage n'a toujours pas
    // répondu : sans le verrou, ça déclencherait un second `fetch` par-dessus.
    await vi.advanceTimersByTimeAsync(5000)
    expect(n).toBe(1)
    // Le premier sondage répond enfin, posant la référence de jiffies.
    differes[0]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 1000, cpu_idle_jiffies: 500 }) })
    await flushPromises()
    // Le minuteur retique : le verrou est levé, un second sondage part bien.
    await vi.advanceTimersByTimeAsync(5000)
    expect(n).toBe(2)
    differes[1]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 750 }) })
    await flushPromises()
    // Le delta n'a pas été corrompu par un chevauchement : 75 % exact, comme
    // dans le test sans chevauchement ci-dessus.
    expect(w.get('[data-system-cpu-usage]').text()).toBe('75 %')
    w.unmount()
  })

  it('pose un repère sur chaque minute pleine de l horloge couverte par le graphe', async () => {
    // Heure système figée à une minute pleine : les repères marquant des
    // instants absolus, leur nombre dépend de la **phase** de la fenêtre par
    // rapport à l'horloge. Sans cette ancre, le test serait tantôt vert
    // tantôt rouge selon l'heure réelle de son exécution.
    vi.setSystemTime(new Date('2026-08-14T12:00:00.000Z'))
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Premier échantillon à 12:00:10 (deux sondages pour un delta), dernier à
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

  it('changer la période ne sonde pas sur-le-champ tant que l échéance court encore', async () => {
    // 5 s par défaut, on avance de 1 s, puis on passe à 10 s : le dernier
    // sondage a 1 s, l'échéance neuve est à 10 s, donc rien ne doit partir
    // avant les 9 s restantes.
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

  it('changer la période sonde tout de suite si elle rend le dernier sondage périmé', async () => {
    // 5 s par défaut, on avance de 4 s, puis on passe à 1 s : le dernier
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

  it('un changement de période pendant un sondage en vol n écrase pas un état plus frais', async () => {
    type Differe = { signal: AbortSignal | null | undefined; resolve: (v: unknown) => void }
    const differes: Differe[] = []
    const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
      if (String(url).includes('/api/i18n')) return Promise.resolve({ ok: true, json: async () => CATALOGUE } as Response)
      // Même raison que dans le test du verrou : le journal est relevé une seule
      // fois au montage, hors du sondage, et n'a rien à faire dans `differes`.
      if (String(url).includes('/api/logs')) return Promise.resolve({ ok: true, json: async () => ({ lines: [] }) } as Response)
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
    // `arreter()` a dû annuler la requête en vol...
    expect(differes[0]!.signal?.aborted).toBe(true)
    // ... sans que cette annulation compte pour une panne : une requête
    // abandonnée par notre propre code n'est pas un échec du cœur.
    expect(w.find('[data-system-unavailable]').exists()).toBe(false)
    // ... et la reprise n'est plus immédiate : le dernier sondage venant tout
    // juste d'être lancé, l'échéance du nouveau rythme (1 s) n'est pas
    // atteinte. C'est elle qui relancera.
    expect(differes.length).toBe(1)
    await vi.advanceTimersByTimeAsync(1000)
    expect(differes.length).toBe(2)
    // La requête annulée finit par « répondre » avec des données pourtant
    // plus anciennes que celles déjà posées par la requête plus fraîche :
    // elle ne doit ni les écraser, ni afficher la ligne d'indisponibilité —
    // une requête abandonnée par notre propre code n'est pas un échec du
    // cœur.
    differes[1]!.resolve({ ok: true, json: async () => payload({ cpu_total_jiffies: 2000, cpu_idle_jiffies: 1000 }) })
    await flushPromises()
    expect(w.find('[data-system-unavailable]').exists()).toBe(false)
    w.unmount()
  })

  it('re-choisir la période déjà active ne redéclenche pas le sondage', async () => {
    const f = stub(payload())
    const w = await monter()
    await flushPromises()
    const appels = f.mock.calls.length
    // La valeur initiale du sélecteur est déjà « 5 » (période par défaut) :
    // la re-choisir ne doit ni sonder immédiatement, ni réinitialiser le
    // minuteur.
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

  it('affiche la charge moyenne dans la carte historique', async () => {
    stub(payload())
    const w = await monter()
    expect(w.get('[data-system-load]').text()).toBe('0.50 · 0.40 · 0.30')
    w.unmount()
  })

  it('affiche un tiret pour la tension quand aucune sonde n est presente', async () => {
    // `null` : pas de capteur `rpi_volt`, à distinguer d'une alimentation
    // saine (`false`) — l'ancien affichage confondait les deux.
    stub(payload({ under_voltage: null }))
    const w = await monter()
    const tension = w.get('[data-system-under-voltage]')
    expect(tension.text()).toBe('—')
    expect(tension.classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('affiche la tension nominale quand la sonde ne detecte rien', async () => {
    stub(payload({ under_voltage: false }))
    const w = await monter()
    const tension = w.get('[data-system-under-voltage]')
    expect(tension.text()).toBe('system_voltage_ok')
    expect(tension.classes()).not.toContain('text-destructive')
    w.unmount()
  })

  it('affiche l antecedent quand la sonde est saine mais un episode a eu lieu depuis le demarrage', async () => {
    // `under_voltage: false` (rien à l'instant) mais `under_voltage_since_boot:
    // true` (le bit collant du micrologiciel) : un troisième état, distinct
    // de la sous-tension en cours, sans le rouge de l'alerte immédiate.
    stub(payload({ under_voltage: false, under_voltage_since_boot: true }))
    const w = await monter()
    const tension = w.get('[data-system-under-voltage]')
    expect(tension.text()).toBe('system_voltage_since_boot')
    expect(tension.classes()).not.toContain('text-destructive')
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

  it('affiche l alerte en rouge en cas de sous-tension detectee', async () => {
    stub(payload({ under_voltage: true }))
    const w = await monter()
    const tension = w.get('[data-system-under-voltage]')
    // Le mot court dans la grille, pas la phrase entière : voir le test
    // suivant pour la phrase de conseil, affichée à part.
    expect(tension.text()).toBe('system_voltage_low')
    expect(tension.classes()).toContain('text-destructive')
    w.unmount()
  })

  it('affiche la phrase de conseil sous la grille seulement quand l alerte est active', async () => {
    stub(payload({ under_voltage: false }))
    const w = await monter()
    expect(w.find('[data-system-under-voltage-avis]').exists()).toBe(false)
    w.unmount()
  })

  it('affiche la phrase de conseil avec role status en cas de sous-tension', async () => {
    stub(payload({ under_voltage: true }))
    const w = await monter()
    const avis = w.get('[data-system-under-voltage-avis]')
    expect(avis.text()).toBe('system_under_voltage')
    expect(avis.attributes('role')).toBe('status')
    w.unmount()
  })

  it('le bouton d aide sur la tension porte un nom accessible et ouvre la popin', async () => {
    stub(payload())
    const w = await monter()
    const bouton = w.get('[data-system-voltage-help]')
    expect(bouton.attributes('aria-label')).toBe('system_voltage_help')
    // Fermée au départ : la popin ne doit pas s'imposer à l'arrivée sur la page.
    expect(document.body.querySelector('[role="dialog"]')).toBeNull()
    await bouton.trigger('click')
    await flushPromises()
    // Montée dans un portail, comme le dialogue d'alimentation.
    expect(document.body.textContent).toContain('system_voltage_help_title')
    expect(document.body.textContent).toContain('system_voltage_help_body')
    w.unmount()
  })

  it('l étiquette de période se corrige quand le catalogue arrive après le montage', async () => {
    // Reproduit l'ordre réel d'un premier chargement : `App.vue` lance le
    // rechargement du catalogue à SON montage, donc la vue se monte avant que
    // la réponse arrive. Tous les libellés se corrigent ensuite d'eux-mêmes,
    // `t` étant une computed — sauf celui du déclencheur du Select, que
    // `SelectValue` sans contenu figeait sur le texte capturé au montage : la
    // liste affichait « 5 system_unit_second » pour toujours.
    //
    // Ce test échouerait donc en rendant `<SelectValue />` sans contenu.
    // `monter()` ne peut pas le voir : il charge le catalogue AVANT de monter.
    stub(payload(), {})
    await useCatalog().reload()
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
    expect(w.get('[data-system-history-span]').text()).toContain('5')
    await w.findComponent(Select).vm.$emit('update:modelValue', '30')
    await flushPromises()
    expect(w.get('[data-system-history-span]').text()).toContain('30')
    w.unmount()
  })

  it('affiche la fenêtre de repli (capacité × période) tant que l historique ne mesure rien', async () => {
    // Page fraîche : aucun échantillon encore poussé (seul le premier sondage
    // de référence a eu lieu), donc rien à mesurer — repli sur la capacité
    // théorique à la période par défaut (5 s × 60 = 5 min).
    stub(payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 }))
    const w = await monter()
    expect(w.get('[data-system-history-span]').text()).toBe('5 min')
    w.unmount()
  })

  it('affiche la durée réelle de l historique plutôt que la capacité une fois mesurable', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Trois sondages supplémentaires à 5 s : le premier ne fait que poser la
    // référence de jiffies, les deux suivants poussent deux échantillons
    // distants de 5 s réels — bien moins que les 5 min que promettrait la
    // capacité théorique à cette période.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    expect(w.get('[data-system-history-span]').text()).toBe('0 min')
    w.unmount()
  })

  describe('survol de l historique', () => {
    /**
     * Cinq réponses successives aux valeurs bien séparées (cpu 10/30/50/70/90 %,
     * ram 5/25/45/65/85 %) : de quoi distinguer sans ambiguïté la colonne
     * pointée. La toute première réponse ne fait que poser la référence de
     * jiffies (voir `utilisationCpu`) et ne pousse aucun échantillon.
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
     * Monte la vue avec les cinq échantillons ci-dessus déjà en historique, et
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

    it('un pointeur au milieu du graphe affiche l échantillon du milieu', async () => {
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 100 })
      const popin = w.get('[data-system-history-popin]')
      expect(popin.text()).toContain('50 %')
      expect(popin.text()).toContain('45 %')
      w.unmount()
    })

    it('un pointeur sur la première colonne affiche le premier échantillon', async () => {
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 0 })
      const popin = w.get('[data-system-history-popin]')
      expect(popin.text()).toContain('10 %')
      expect(popin.text()).toContain('5 %')
      w.unmount()
    })

    it('un pointeur sur la dernière colonne affiche le dernier échantillon', async () => {
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

    it('un appui tactile affiche le popin sans attendre un mouvement', async () => {
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
      // `LARGEUR` du viewBox vaut 100, n = 5 : le pas entre colonnes vaut
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
      // pair, par exemple) ne donnerait pas forcément.
      await svg.trigger('pointermove', { clientX: 125 })
      ligne = w.get('[data-system-history-line]')
      expect(ligne.attributes('x1')).toBe('75')
      w.unmount()
    })

    it('le popin est centré par une transformation constante, bornée en pixels sur les trois régimes', async () => {
      // Graphe large de 200 px (voir le stub de `getBoundingClientRect`
      // ci-dessus), popin large de 100 px (`LARGEUR_POPIN_PX`) : le centre
      // idéal ne peut descendre sous 50 px ni dépasser 150 px sans faire
      // déborder le popin de la carte.
      const { w, svg } = await monterAvecHistorique()
      // Première colonne (i = 0 sur 5) : centre idéal à 0 px, borné à 50 px —
      // la transformation reste -50 % constante, c'est la position qui est
      // bornée, pas un cas particulier de transformation comme avant cette
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

    it('survoler un graphe encore vide n affiche ni popin ni trait', async () => {
      // Un seul sondage : la référence de jiffies est posée, aucun échantillon
      // poussé. Le graphe est là quand même (il l'est désormais toujours, pour
      // que la mise en page ne saute pas), donc il est **survolable** avant
      // d'avoir la moindre donnée — ce que l'ancienne version rendait
      // impossible en ne le dessinant pas. Le garde `< 2` de `survolPointeur`
      // et celui de `xLigneSurvol` deviennent donc porteurs : ce test les
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

  describe('dernières erreurs', () => {
    it('rend une ligne par entrée de journal, dans l’ordre reçu', async () => {
      // `/api/logs` rend déjà les plus récentes en premier (le cœur inverse son
      // tampon), la vue ne retrie pas : elle doit rendre l'ordre tel quel.
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

    it('un journal injoignable ne prive pas la page de ses métriques', async () => {
      // Les deux relevés sont indépendants, chacun avec son `.catch` : un
      // `/api/logs` en panne ne doit pas faire passer la machine pour muette —
      // ce sont justement les métriques qu'on regarde quand le journal manque.
      stub(payload(), CATALOGUE, undefined)
      const w = await monter()
      expect(w.findAll('[data-log-line]')).toHaveLength(0)
      expect(w.find('[data-system-unavailable]').exists()).toBe(false)
      expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
      w.unmount()
    })

    it('le journal n est relevé qu au montage, hors du sondage périodique', async () => {
      // Greffer le journal sur `sonder()` allongerait la prise du verrou « en
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

    it('le bouton annonce le total et n apparaît qu au-delà de la carte', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      expect(w.get('[data-logs-all]').text()).toContain('12')
      w.unmount()

      // Trois erreurs : la carte les montre déjà toutes, une popin n'aurait
      // rien de plus à dire.
      stub(payload(), CATALOGUE, { lines: DOUZE.slice(0, 3) })
      const peu = await monter()
      expect(peu.find('[data-logs-all]').exists()).toBe(false)
      peu.unmount()
    })

    it('la popin liste tout le journal', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // La popin est rendue dans un portail : elle vit dans document.body.
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('12 / 12')
      w.unmount()
    })

    it('le champ filtre la liste et met à jour le compteur', async () => {
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

    it('relève le journal à l ouverture', async () => {
      const f = stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      const avant = f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // Une requête de plus, sur geste utilisateur : le journal reste hors du
      // sondage périodique (verrou « en vol » et delta CPU de `sonder`).
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

      // Fermeture par le bouton du dialogue — le vrai geste, et le seul
      // `[data-slot="dialog-close"]` présent puisque seul le dialogue ouvert
      // est rendu dans le portail. Puis réouverture : le champ repart vide,
      // sinon la popin s'ouvrirait sur une liste tronquée sans que rien à
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
