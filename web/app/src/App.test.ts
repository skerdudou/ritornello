import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App.vue'
import { router } from './router'

// Le marqueur visuel de la page courante : c'est la classe que
// `exact-active-class` ajoute au seul lien exact. L'épingler telle quelle est
// volontaire — c'est le mécanisme du soulignement, et l'échanger contre autre
// chose doit faire échouer ce test plutôt que passer inaperçu.
const SOULIGNE = 'after:scale-x-100'

const CATALOGUE = { config_title: 'Configuration', system_title: 'Système' }

/** `/api/i18n` d'un côté, `/api/status` de l'autre — la nav ne lit rien d'autre. */
function stub(plugins = [{ name: 'radio', admin: true }]) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockImplementation((url: string) =>
      Promise.resolve({
        ok: true,
        json: async () => (String(url).includes('/api/i18n') ? CATALOGUE : { plugins }),
      } as Response),
    ),
  )
}

/**
 * Monte le shell sur `chemin`. `RouterView` est bouché : seule la nav est en
 * cause ici, et monter les vues réelles ferait ouvrir à `HomeView` son flux
 * `EventSource` que jsdom n'implémente pas.
 */
async function monter(chemin: string) {
  stub()
  await router.push(chemin)
  await router.isReady()
  const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
  await flushPromises()
  return w
}

describe('navigation du shell', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('ne souligne que le lien de la page courante', async () => {
    const w = await monter('/system')
    expect(w.get('a[href="/system"]').classes()).toContain(SOULIGNE)
    expect(w.get('a[href="/config"]').classes()).not.toContain(SOULIGNE)
    // Le lien de l'accueil est celui qui rendrait `active-class` inutilisable :
    // la correspondance inclusive du routeur le tient pour actif sur toutes les
    // pages, `/` étant un préfixe de tout. D'où `exact-active-class`.
    expect(w.get('a[href="/"]').classes()).not.toContain(SOULIGNE)
    w.unmount()
  })

  it('souligne la marque sur l accueil', async () => {
    // Sans elle, l'accueil serait la seule page sans rien de souligné : c'est
    // le lien de l'accueil autant que la marque.
    const w = await monter('/')
    expect(w.get('a[href="/"]').classes()).toContain(SOULIGNE)
    expect(w.get('a[href="/system"]').classes()).not.toContain(SOULIGNE)
    w.unmount()
  })

  it('souligne le lien d un plugin admin sur sa page', async () => {
    const w = await monter('/plugins/radio/')
    expect(w.get('a[href="/plugins/radio/"]').classes()).toContain(SOULIGNE)
    expect(w.get('a[href="/"]').classes()).not.toContain(SOULIGNE)
    w.unmount()
  })

  it('marque la page courante pour les lecteurs d écran, pas seulement à l œil', async () => {
    // `aria-current="page"` vient de `RouterLink` lui-même : le soulignement
    // double cette sémantique, il ne la remplace pas. Le test la fixe pour que
    // personne ne « simplifie » la nav en liens nus plus tard.
    const w = await monter('/config')
    expect(w.get('a[href="/config"]').attributes('aria-current')).toBe('page')
    expect(w.get('a[href="/system"]').attributes('aria-current')).toBeUndefined()
    w.unmount()
  })
})
