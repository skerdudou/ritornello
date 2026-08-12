import { Select, SelectItem, toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

// Meme approche que `useTheme.test.ts` : on garde le vrai module (composants,
// `api`, ...) et on remplace uniquement les deux entrees de `toast` que cette
// vue utilise, pour pouvoir les observer sans afficher de notification.
vi.mock('@ritornello/ui', async () => {
  const reel = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...reel, toast: { ...reel.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOGUE = {
  config_title: 'Configuration',
  plugins_title: 'Plugins',
  col_plugin: 'Plugin', col_kind: 'Genre', col_state: 'État', col_admin: 'Admin',
  connected: 'connecté', unavailable: 'indisponible', admin_link: 'admin',
  audio_output: 'Sortie audio', language: 'Langue', change: 'Changer', ok: 'OK',
  recent_errors: 'Dernières erreurs',
  startup_title: 'Démarrage', startup_on: 'allumé', startup_standby: 'veille',
  volume_hold_title: 'Volume maintenu',
  volume_hold_initial: 'Délai initial (ms)', volume_hold_interval: 'Intervalle de répétition (ms)',
}

/** Charges utiles servies par le faux `fetch`, surchargeables par test. */
function charges() {
  return {
    '/api/status': {
      plugins: [
        { name: 'radio', kind: 'source', connected: true, admin: true },
        { name: 'cd', kind: 'source', connected: false, admin: false },
      ],
      active_source: 'radio',
    } as unknown,
    '/api/audio-output': { devices: ['hw:CARD=Headphones', 'hw:CARD=HDMI'], current: 'hw:CARD=HDMI' } as unknown,
    '/api/locale': { locales: ['en', 'fr'], current: 'fr' } as unknown,
    '/api/logs': { lines: ['WARN plugin radio indisponible'] } as unknown,
    '/api/settings': { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false } as unknown,
    '/api/i18n': CATALOGUE as unknown,
  }
}

type Charges = ReturnType<typeof charges>

/**
 * Monte ConfigView avec un routeur en memoire (RouterLink est importe
 * directement par le SFC : il lui faut un vrai routeur, ce qui permet en outre
 * d'observer le `href` reellement resolu) et un `fetch` espionne.
 */
async function monter(surcharges: Partial<Charges> = {}, erreurPut?: string) {
  const table = { ...charges(), ...surcharges }
  const puts: Array<{ url: string; corps: unknown }> = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      puts.push({ url, corps: JSON.parse(String(init.body)) })
      if (erreurPut) {
        return new Response(JSON.stringify({ error: erreurPut }), { status: 422 })
      }
      return new Response(null, { status: 204 })
    }
    const data = (table as Record<string, unknown>)[url]
    if (data === undefined) return new Response('inconnu', { status: 404 })
    return new Response(JSON.stringify(data), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)

  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: { template: '<div />' } },
      { path: '/config', component: { template: '<div />' } },
      { path: '/plugins/:name/', component: { template: '<div />' } },
    ],
  })
  router.push('/config')
  await router.isReady()

  const ConfigView = (await import('./ConfigView.vue')).default
  const w = mount(ConfigView, { global: { plugins: [router] } })
  await flushPromises()
  return { w, spy, puts, table }
}

function reinitialiser() {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
}

// La plus grande surface migree du chantier n'avait aucun test unitaire :
// quatre tests Rust couvraient l'ancienne page rendue cote serveur et ont ete
// supprimes avec elle (IMPORTANT 7 de la revue finale). Rien n'exercait plus
// les colonnes de la table des plugins, les libelles connecte/indisponible, la
// forme de l'URL du lien d'admin, le PUT audio, le PUT de langue suivi du
// rechargement des catalogues, ni le rendu des journaux — et le defaut de la
// sortie audio vide (IMPORTANT 3) est precisement celui qu'un tel test aurait
// attrape.
describe('ConfigView — table des plugins', () => {
  beforeEach(reinitialiser)

  it('rend une ligne par plugin avec ses quatre colonnes', async () => {
    const { w } = await monter()
    const lignes = w.findAll('[data-plugin-row]')
    expect(lignes).toHaveLength(2)
    expect(lignes[0]!.find('[data-plugin-name]').text()).toBe('radio')
    expect(lignes[0]!.find('[data-plugin-kind]').text()).toBe('source')
    expect(lignes[1]!.find('[data-plugin-name]').text()).toBe('cd')
    expect(lignes[1]!.find('[data-plugin-kind]').text()).toBe('source')
    // Les quatre en-tetes sont traduits depuis le catalogue du coeur.
    const entetes = w.findAll('th').map((h) => h.text())
    expect(entetes).toEqual(['Plugin', 'Genre', 'État', 'Admin'])
  })

  it('distingue l’état connecté de l’état indisponible', async () => {
    const { w } = await monter()
    const lignes = w.findAll('[data-plugin-row]')
    expect(lignes[0]!.find('[data-plugin-state]').text()).toBe('connecté')
    expect(lignes[1]!.find('[data-plugin-state]').text()).toBe('indisponible')
  })

  it('ne rend le lien d’admin que pour les plugins admin, sur /plugins/<nom>/', async () => {
    const { w } = await monter()
    const lignes = w.findAll('[data-plugin-row]')
    const lien = lignes[0]!.find('[data-admin-link]')
    expect(lien.exists()).toBe(true)
    // La forme canonique avec slash final : c'est l'URL historique, epinglee
    // aussi cote coeur (`serves_shell("/plugins/radio/")`) et desormais la
    // seule que le routeur laisse vivre.
    expect(lien.attributes('href')).toBe('/plugins/radio/')
    expect(lien.text()).toBe('admin')
    // « cd » n'est pas admin : pas de lien, un tiret a la place.
    expect(lignes[1]!.find('[data-admin-link]').exists()).toBe(false)
    expect(lignes[1]!.text()).toContain('-')
  })

  it('une table de plugins vide ne casse pas le rendu', async () => {
    const { w } = await monter({ '/api/status': { plugins: [], active_source: '' } })
    expect(w.findAll('[data-plugin-row]')).toHaveLength(0)
    expect(w.text()).toContain('Plugins')
  })
})

describe('ConfigView — langue', () => {
  beforeEach(reinitialiser)

  it('envoie le PUT de langue puis recharge le catalogue', async () => {
    // Le changement de langue recharge les catalogues au lieu de recharger la
    // page entiere comme le faisait l'ancienne IHM : c'est `chargerTout()` (et
    // son `reload()`) qui remplace `location.reload()`. Le test verifie donc
    // qu'un second `GET /api/i18n` suit bien le PUT.
    const { w, spy, puts } = await monter()
    const avant = spy.mock.calls.filter((c) => c[0] === '/api/i18n').length
    expect(avant).toBeGreaterThan(0) // charge au montage

    await w.findAllComponents(Select)[1]!.vm.$emit('update:modelValue', 'en')
    await w.find('[data-lang-change]').trigger('click')
    await flushPromises()

    expect(puts).toEqual([{ url: '/api/locale', corps: { locale: 'en' } }])
    // Le catalogue a ete relu apres le PUT — sans quoi l'IHM resterait
    // affichee dans l'ancienne langue jusqu'au prochain rechargement manuel.
    expect(spy.mock.calls.filter((c) => c[0] === '/api/i18n').length).toBeGreaterThan(avant)
  })

  it('affiche le nom de la langue et non son code', async () => {
    // « français » se lit, « fr » se devine. Le code reste la valeur envoyée au
    // cœur (verifié par le test du PUT ci-dessus).
    const { w } = await monter()
    const textes = w.findAllComponents(SelectItem).map((i) => i.text())
    expect(textes).toContain('Français')
    expect(textes).toContain('English')
    expect(textes).not.toContain('fr')
  })

  it('un PUT de langue en échec est signalé et ne recharge rien', async () => {
    const { w, spy } = await monter({}, 'langue inconnue')
    const avant = spy.mock.calls.filter((c) => c[0] === '/api/i18n').length
    await w.find('[data-lang-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('langue inconnue')
    // Aucun rechargement : la langue n'a pas change cote serveur, relire les
    // catalogues ne ferait que masquer l'echec derriere une IHM inchangee.
    expect(spy.mock.calls.filter((c) => c[0] === '/api/i18n').length).toBe(avant)
  })
})

describe('ConfigView — journaux', () => {
  beforeEach(reinitialiser)

  it('rend une ligne par entrée de journal, dans l’ordre reçu', async () => {
    // `/api/logs` renvoie deja les plus recentes en premier (le cœur inverse le
    // tampon), la vue ne retrie pas : elle doit rendre l'ordre tel quel.
    const { w } = await monter({
      '/api/logs': { lines: ['WARN la plus recente', 'WARN la plus ancienne'] },
    })
    const lignes = w.findAll('[data-log-line]').map((l) => l.text())
    expect(lignes).toEqual(['WARN la plus recente', 'WARN la plus ancienne'])
  })

  it('aucune erreur récente : aucune ligne, et la carte reste rendue', async () => {
    const { w } = await monter({ '/api/logs': { lines: [] } })
    expect(w.findAll('[data-log-line]')).toHaveLength(0)
    expect(w.text()).toContain('Dernières erreurs')
  })

  it('une route injoignable ne casse pas la page', async () => {
    // Chaque `api.get` a son `.catch` : un `/api/logs` en erreur ne doit pas
    // priver l'utilisateur de la table des plugins, seul moyen de diagnostiquer
    // justement ce genre de panne.
    const { w } = await monter({ '/api/logs': undefined })
    expect(w.findAll('[data-plugin-row]')).toHaveLength(2)
    expect(w.findAll('[data-log-line]')).toHaveLength(0)
  })
})

describe('ConfigView — sortie audio', () => {
  beforeEach(reinitialiser)

  it('envoie le PUT de sortie audio avec la charge utile attendue', async () => {
    const { w, puts } = await monter()
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=HDMI')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: 'hw:CARD=HDMI' } }])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('sans sortie choisie, retombe sur le premier périphérique disponible', async () => {
    // IMPORTANT 3 de la revue finale. L'ancienne page etait rendue cote
    // serveur : faute de sortie choisie, aucun `<option>` ne portait
    // `selected`, donc le navigateur selectionnait le premier peripherique et
    // « Changer » envoyait toujours un nom reel. Avec `?? ''`, une
    // installation neuve (`current: null`) laissait le declencheur vide et
    // « Changer » envoyait `device: ""` — que le coeur stockait alors sans
    // validation : `current: ""` renvoye indefiniment, `""` transmis a mpv
    // puis persiste, et un toast de succes par-dessus.
    const { w, puts } = await monter({
      '/api/audio-output': { devices: ['hw:CARD=Headphones', 'hw:CARD=HDMI'], current: null },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=Headphones')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    // Jamais la chaine vide : c'est tout l'objet du repli.
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: 'hw:CARD=Headphones' } }])
  })

  it('sans aucun périphérique listé, ne fabrique pas de nom', async () => {
    // Cas degenere (aucune sortie ALSA visible) : le repli n'a rien sur quoi
    // retomber. La vue ne doit pas inventer de valeur ; le coeur refuserait de
    // toute facon la chaine vide par un 422.
    const { w } = await monter({ '/api/audio-output': { devices: [], current: null } })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('')
  })
})

describe('ConfigView — réglages', () => {
  beforeEach(reinitialiser)

  it('affiche les réglages lus depuis /api/settings', async () => {
    const { w } = await monter({
      '/api/settings': { volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, start_in_standby: true },
    })
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('800')
    expect((w.find('[data-hold-interval]').element as HTMLInputElement).value).toBe('250')
    // Le sélecteur de démarrage reflète la veille.
    const demarrage = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'standby')
    expect(demarrage).toBeDefined()
  })

  it('enregistre le démarrage en veille par un PUT du bloc complet', async () => {
    const { w, puts } = await monter()
    const demarrage = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'on')!
    await demarrage.vm.$emit('update:modelValue', 'standby')
    await w.find('[data-startup-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        corps: { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: true },
      },
    ])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('enregistre les délais du volume maintenu en nombres', async () => {
    const { w, puts } = await monter()
    await w.find('[data-hold-initial]').setValue('1500')
    await w.find('[data-hold-interval]').setValue('300')
    await w.find('[data-hold-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        corps: { volume_repeat_initial_ms: 1500, volume_repeat_interval_ms: 300, start_in_standby: false },
      },
    ])
  })

  it('un PUT de réglages refusé est signalé par un toast', async () => {
    const { w } = await monter({}, 'délai initial hors bornes (200-5000 ms)')
    await w.find('[data-hold-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('délai initial hors bornes (200-5000 ms)')
  })

  it('un /api/settings injoignable laisse les valeurs par défaut', async () => {
    const { w } = await monter({ '/api/settings': undefined })
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('1000')
  })
})
