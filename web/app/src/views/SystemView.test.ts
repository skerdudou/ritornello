import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useCatalog } from '../composables/useCatalog'
import SystemView from './SystemView.vue'

// Charge utile complète, réutilisée en la modifiant par cas.
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
    ...surcharge,
  }
}

/** Catalogue minimal : seules les unités sont assertées à l'affichage. */
const CATALOGUE = {
  system_unit_mb: 'Mo',
  system_unit_gb: 'Go',
  system_unit_day: 'j',
  system_unit_hour: 'h',
  system_unit_minute: 'min',
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
    stub(payload())
    const w = await monter()
    // Un seul échantillon : pas de ligne, le message d'attente à la place.
    expect(w.find('[data-system-history-empty]').exists()).toBe(true)
    expect(w.find('[data-system-history]').exists()).toBe(false)
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.find('[data-system-history]').exists()).toBe(true)
    expect(w.get('[data-system-history]').html()).toContain('M0.00,')
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

  it('reprend la main quand le redémarrage du service aboutit', async () => {
    // Uptime décroissant : le service est bien revenu, ce qu'une simple
    // réponse ne prouverait pas (le premier sondage peut encore atteindre
    // l'ancien process).
    const reponses = [payload(), payload(), payload({ service_uptime_s: 2 })]
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
})
