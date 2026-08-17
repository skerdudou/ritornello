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
  audio_output: 'Sortie audio', audio_default_device: 'Par défaut (système)',
  language: 'Langue', change: 'Changer', ok: 'OK',
  recent_errors: 'Dernières erreurs',
  startup_title: 'Démarrage', startup_on: 'allumé', startup_standby: 'veille',
  volume_hold_title: 'Volume maintenu',
  volume_hold_initial: 'Délai initial (ms)', volume_hold_interval: 'Intervalle de répétition (ms)',
  overlays_title: 'Incrustations',
  overlay_ms_label: "Durée d'affichage (volume, messages) (ms)",
  tens_window_ms_label: 'Fenêtre de saisie du cumul +10 (ms)',
  toc_label: 'sections',
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
    '/api/audio-output': {
      devices: [
        { name: 'hw:CARD=Headphones', description: 'bcm2835 Headphones — Direct hardware device' },
        { name: 'hw:CARD=HDMI', description: '' },
      ],
      current: 'hw:CARD=HDMI',
    } as unknown,
    '/api/locale': { locales: ['en', 'fr'], current: 'fr' } as unknown,
    '/api/logs': { lines: ['WARN plugin radio indisponible'] } as unknown,
    '/api/settings': {
      volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false,
      overlay_ms: 5000, tens_window_ms: 5000,
    } as unknown,
    '/api/i18n': CATALOGUE as unknown,
  }
}

type Charges = ReturnType<typeof charges>

// jsdom n'implemente pas IntersectionObserver : la vue en a besoin pour le
// scrollspy, on la remplace par une fausse classe qui capture le callback
// pour que les tests puissent simuler des entrees/sorties de viewport.
type IOCallback = (entries: Array<{ target: Element; isIntersecting: boolean }>) => void
let ioCallback: IOCallback | null = null
class FauxIO {
  constructor(cb: IOCallback) { ioCallback = cb }
  observe() {}
  disconnect() {}
}

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
  vi.stubGlobal('IntersectionObserver', FauxIO)

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
  // Attache au vrai document (et non a un noeud detache, defaut de `mount`) :
  // le sommaire retrouve ses sections via `document.getElementById`, qui ne
  // voit rien hors de l'arbre du document. On repart d'un corps vide a chaque
  // montage pour eviter que les id des sections (uniques par charge utile,
  // mais reutilises entre tests) ne pointent vers le montage precedent.
  document.body.innerHTML = ''
  const w = mount(ConfigView, { global: { plugins: [router] }, attachTo: document.body })
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

  it('envoie le PUT du périphérique choisi, inchangé', async () => {
    const { w, puts } = await monter()
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=HDMI')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: 'hw:CARD=HDMI' } }])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('sans choix enregistré, l’entrée par défaut est sélectionnée et « Changer » envoie null', async () => {
    // Fini le repli sur le premier périphérique : `current: null` est un état
    // légitime (« suis le défaut système »), l'entrée synthétique le porte.
    const { w, puts } = await monter({
      '/api/audio-output': {
        devices: [{ name: 'hw:CARD=Headphones', description: '' }],
        current: null,
      },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('__system_default__')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: null } }])
  })

  it('l’entrée par défaut est la première de la liste', async () => {
    const { w } = await monter()
    const premier = w.findAllComponents(SelectItem)[0]!
    expect(premier.attributes('data-audio-default')).toBeDefined()
    expect(premier.text()).toBe('Par défaut (système)')
  })

  it('affiche la description en principal et le nom technique en secondaire', async () => {
    const { w } = await monter()
    const items = w.findAllComponents(SelectItem)
    const avecDescription = items.find((i) => i.text().includes('bcm2835 Headphones'))!
    expect(avecDescription.text()).toContain('hw:CARD=Headphones')
    // Sans description : le nom seul, pas de ligne secondaire vide.
    const sansDescription = items.find((i) => i.props('value') === 'hw:CARD=HDMI')!
    expect(sansDescription.text()).toBe('hw:CARD=HDMI')
  })

  it('un périphérique choisi mais absent de la liste reste visible', async () => {
    // Carte débranchée : la sélection courante est rajoutée en fin de liste
    // (nom seul) plutôt que de laisser un déclencheur vide.
    const { w } = await monter({
      '/api/audio-output': {
        devices: [{ name: 'hw:CARD=Headphones', description: '' }],
        current: 'hw:CARD=USB',
      },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=USB')
    const valeurs = w.findAllComponents(SelectItem).map((i) => i.props('value'))
    expect(valeurs).toContain('hw:CARD=USB')
  })

  it('aucun périphérique listé : l’entrée par défaut reste utilisable', async () => {
    const { w, puts } = await monter({ '/api/audio-output': { devices: [], current: null } })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('__system_default__')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: null } }])
  })

  it('un /api/audio-output injoignable désactive « Changer »', async () => {
    // Sans cela, le sélecteur affiche « Par défaut (système) » comme si
    // c'était l'état réel, et « Changer » enverrait device: null — une
    // réinitialisation silencieuse.
    const { w } = await monter({ '/api/audio-output': undefined })
    expect(w.find('[data-audio-change]').attributes('disabled')).toBeDefined()
  })
})

describe('ConfigView — réglages', () => {
  beforeEach(reinitialiser)

  it('affiche les réglages lus depuis /api/settings', async () => {
    const { w } = await monter({
      '/api/settings': {
        volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, start_in_standby: true,
        overlay_ms: 5000, tens_window_ms: 5000,
      },
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
        corps: {
          volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: true,
          overlay_ms: 5000, tens_window_ms: 5000,
        },
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
        corps: {
          volume_repeat_initial_ms: 1500, volume_repeat_interval_ms: 300, start_in_standby: false,
          overlay_ms: 5000, tens_window_ms: 5000,
        },
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
    // Même valeur que le `Default` de `Settings` côté cœur (state.rs) : les
    // deux replis doivent rester alignés, sinon la page affiche brièvement
    // autre chose que ce que l'appareil applique.
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('800')
  })
})

describe('ConfigView — incrustations', () => {
  beforeEach(reinitialiser)

  it('affiche les deux durées lues depuis /api/settings', async () => {
    const { w } = await monter({
      '/api/settings': {
        volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, start_in_standby: false,
        overlay_ms: 3000, tens_window_ms: 9000,
      },
    })
    expect((w.find('[data-overlay-ms]').element as HTMLInputElement).value).toBe('3000')
    expect((w.find('[data-tens-window-ms]').element as HTMLInputElement).value).toBe('9000')
  })

  it('enregistre les deux durées en nombres, au bloc complet', async () => {
    const { w, puts } = await monter()
    await w.find('[data-overlay-ms]').setValue('2000')
    await w.find('[data-tens-window-ms]').setValue('7000')
    await w.find('[data-overlays-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        corps: {
          volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false,
          overlay_ms: 2000, tens_window_ms: 7000,
        },
      },
    ])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('un PUT hors bornes est signalé par un toast', async () => {
    const { w } = await monter({}, 'incrustation hors bornes (1000-15000 ms)')
    await w.find('[data-overlays-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('incrustation hors bornes (1000-15000 ms)')
  })
})

describe('ConfigView — sommaire', () => {
  beforeEach(reinitialiser)

  it('liste une entrée par section, avec le libellé de sa carte', async () => {
    const { w } = await monter()
    const liens = w.findAll('[data-toc-link]')
    expect(liens.map((l) => l.text())).toEqual([
      'Plugins', 'Sortie audio', 'Langue', 'Démarrage', 'Volume maintenu', 'Incrustations', 'Dernières erreurs',
    ])
    // Masqué sur petit écran : la colonne suit la largeur du shell, il n'y a
    // pas la place en mobile.
    expect(w.find('[data-toc]').classes()).toContain('hidden')
  })

  it('un clic fait défiler en douceur vers la section et la marque active', async () => {
    const { w } = await monter()
    const scrollIntoView = vi.fn()
    const cible = w.find('#audio')
    expect(cible.exists()).toBe(true)
    cible.element.scrollIntoView = scrollIntoView
    await w.findAll('[data-toc-link]')[1]!.trigger('click')
    expect(scrollIntoView).toHaveBeenCalledWith({ behavior: 'smooth' })
    expect(w.findAll('[data-toc-link]')[1]!.attributes('aria-current')).toBe('true')
  })

  it('le défilement met à jour la section active (scrollspy)', async () => {
    const { w } = await monter()
    expect(ioCallback).not.toBeNull()
    ioCallback!([{ target: w.find('#language').element, isIntersecting: true }])
    ioCallback!([{ target: w.find('#plugins').element, isIntersecting: false }])
    await w.vm.$nextTick()
    const actifs = w.findAll('[data-toc-link][aria-current="true"]')
    expect(actifs).toHaveLength(1)
    expect(actifs[0]!.text()).toBe('Langue')
  })
})
