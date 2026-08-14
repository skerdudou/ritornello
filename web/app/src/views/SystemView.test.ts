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
    uptime_s: 90_061,
    service_uptime_s: 3_600,
    hostname: 'ritornello',
    ip: '192.168.1.20',
    os: 'Debian GNU/Linux 12 (bookworm)',
    kernel: '6.6.51+rpt-rpi-v7',
    version: '0.1.0',
    can_power_off: true,
    can_reboot: true,
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

/** Catalogue minimal : les unités et le gabarit de la fenêtre d'historique
 *  sont assertés à l'affichage. */
const CATALOGUE = {
  system_unit_mb: 'Mo',
  system_unit_gb: 'Go',
  system_unit_day: 'j',
  system_unit_hour: 'h',
  system_unit_minute: 'min',
  system_history_span: '{minutes} min',
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
function stub(corps: unknown | (() => unknown), catalogue: Record<string, string> = CATALOGUE) {
  const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
    }
    const j = String(url).includes('/api/i18n')
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
    // Un seul échantillon : pas de ligne, le message d'attente à la place.
    expect(w.find('[data-system-history-empty]').exists()).toBe(true)
    expect(w.find('[data-system-history]').exists()).toBe(false)
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

  it('affiche l alerte en rouge en cas de sous-tension detectee', async () => {
    stub(payload({ under_voltage: true }))
    const w = await monter()
    const tension = w.get('[data-system-under-voltage]')
    expect(tension.text()).toBe('system_under_voltage')
    expect(tension.classes()).toContain('text-destructive')
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

    it('le popin reste dans la carte sur la première et la dernière colonne', async () => {
      const { w, svg } = await monterAvecHistorique()
      await svg.trigger('pointermove', { clientX: 0 })
      const debut = w.get('[data-system-history-popin]').element as HTMLElement
      expect(debut.style.left).toBe('0%')
      expect(debut.style.transform).toBe('translateX(0)')
      await svg.trigger('pointermove', { clientX: 200 })
      const fin = w.get('[data-system-history-popin]').element as HTMLElement
      expect(fin.style.left).toBe('100%')
      expect(fin.style.transform).toBe('translateX(-100%)')
      w.unmount()
    })

    it('rien ne s affiche tant que moins de deux échantillons existent', async () => {
      // Un seul sondage : la référence de jiffies est posée mais aucun
      // échantillon poussé, le graphe lui-même n'est pas dessiné.
      stub(payload({ cpu_total_jiffies: 0, cpu_idle_jiffies: 0 }))
      const w = await monter()
      expect(w.find('[data-system-history]').exists()).toBe(false)
      expect(w.find('[data-system-history-popin]').exists()).toBe(false)
      w.unmount()
    })
  })
})
