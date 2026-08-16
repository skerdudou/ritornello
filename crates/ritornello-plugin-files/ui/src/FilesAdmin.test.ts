import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOGUE, monter, serveur } from './harnais'

describe('FilesAdmin, la page', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(() => vi.useRealTimers())

  it('adresse toutes ses requêtes sous le préfixe absolu reçu par `base`', async () => {
    // Régression encodée : un `./api/data` relatif se résout contre l'URL du
    // navigateur, pas contre le préfixe du plugin. Sur `/plugins/files` (sans
    // slash final, forme que le routeur du shell accepte aussi) il désignerait
    // `/plugins/api/data` — que le cœur interprète comme un plugin nommé
    // « api » : 404, page vide, tous les boutons en échec.
    const { w, s } = await monter({ saved: [{ name: 'Jazz', where: 'internal' }] })
    await w.find('[data-load-playlist]').trigger('click')
    await flushPromises()
    expect(s.urls().length).toBeGreaterThan(1)
    for (const u of s.urls()) expect(u).toBe(`${BASE}api/data`)
  })

  it('affiche un refus du serveur verbatim, sans le reformuler', async () => {
    // Les refus sont produits par les catalogues i18n du **serveur** : ils sont
    // déjà traduits, et leur substituer un message maison ferait perdre le
    // détail (nom de racine, plafond dépassé) qui les rend actionnables.
    const { w, s } = await monter({ playlist: [{ path: 'a.mp3', name: 'A' }] })
    s.refus = 'invalid root name "Mon NAS" : lettres minuscules, chiffres et tirets seulement'
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(w.find('[data-message]').text()).toBe(s.refus)
  })

  it('affiche la sortie de systemctl telle quelle, sauts de ligne compris', async () => {
    // L'échec de `{"op":"mount"}` porte la sortie brute de `systemctl` : c'est
    // elle qui est actionnable. La replier dans un paragraphe la rendrait
    // illisible, et la reformuler la détruirait — d'où le rendu dans un `pre`.
    const sortie =
      'Job for ritornello-media-mount.service failed.\n' +
      'See "systemctl status ritornello-media-mount.service" and "journalctl -xeu ...".'
    // Le réessai n'existe que si un montage a déjà échoué : le montage suit
    // désormais la déclaration, il n'y a plus de bouton « Monter » permanent à
    // aller chercher.
    const { w, s } = await monter({
      roots: [{ name: 'nas', kind: 'smb', host: 'h', share: 's', mounted: false }],
      mount_error: sortie,
    })
    s.refus = sortie
    await w.find('[data-retry-mount]').trigger('click')
    await flushPromises()
    const pre = w.find('[data-message]')
    expect(pre.element.tagName).toBe('PRE')
    expect(pre.element.textContent).toBe(sortie)
  })

  it('sonde pendant un balayage et cesse dès qu’il se termine', async () => {
    // Le protocole d'admin ne pousse **rien** : ni canal d'événements ni
    // websocket derrière le socket d'admin. Sans ce sondage, un `add_dir` —
    // asynchrone côté plugin — n'afficherait jamais son avancement, et la liste
    // n'apparaîtrait qu'au prochain rechargement manuel de la page.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = serveur({ scan: { running: true, found: 12, dir: 'Albums/Jazz' } })
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(w.find('[data-scan]').text()).toBe('Balayage de Albums/Jazz — 12 pistes trouvées')
    const gets = () => s.spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method !== 'PUT').length
    expect(gets()).toBe(1)

    s.data.scan = { running: true, found: 300, dir: 'Albums/Rock' }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(2)
    expect(w.find('[data-scan]').text()).toContain('300')

    // Fin du balayage : le sondage doit s'arrêter de lui-même, sinon la page
    // martèle le plugin une fois par seconde jusqu'à sa fermeture.
    s.data.scan = { running: false, found: 300, dir: '' }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(gets()).toBe(3)
    expect(w.find('[data-scan]').exists()).toBe(false)

    vi.advanceTimersByTime(5000)
    await flushPromises()
    expect(gets()).toBe(3)
  })

  it('montre l’incident du dernier balayage, qui survit à sa fin', async () => {
    // Régression encodée : `add_dir` rend la main **avant** la fin de la marche
    // récursive, donc son accusé de réception ne dit rien de son issue. Si la
    // page n'affichait pas `scan.error`, un ajout parti en échec passerait pour
    // un ajout qui n'a simplement rien trouvé.
    const refus = 'this folder holds more than 10000 tracks: narrow it down'
    const { w } = await monter({ scan: { running: false, found: 0, dir: '', error: refus } })
    expect(w.find('[data-scan-error]').element.textContent).toBe(refus)
    expect(w.find('[data-scan]').exists()).toBe(false)
  })

  it('n’émet plus rien après démontage, même en plein balayage', async () => {
    // Sans `onUnmounted`, la minuterie survit au composant : le shell change de
    // page et un `recharger()` continue de tourner chaque seconde contre un
    // composant mort.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = serveur({ scan: { running: true, found: 1, dir: 'a' } })
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    const avant = s.spy.mock.calls.length
    w.unmount()
    vi.advanceTimersByTime(5000)
    await flushPromises()
    expect(s.spy.mock.calls.length).toBe(avant)
  })

  it('premier chargement en échec : la page est inerte et n’écrit rien', async () => {
    // Régression encodée, du même ordre que celle de la page radio : après un
    // GET en échec, `roots` est vide alors que `media-roots.toml` ne l'est pas.
    // Un « Enregistrer les racines » enverrait `{op:'save_roots', roots: []}`,
    // qui écrase le fichier — tous les partages déclarés disparaissent, sans
    // confirmation ni retour arrière.
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(null, { status: 204 })
      return new Response('indisponible', { status: 503 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(w.find('[data-message]').text()).toContain('Erreur : ')
    // Les volets ne sont même pas montés : il n'y a rien de vrai à montrer.
    expect(w.find('[data-volet-sources]').exists()).toBe(false)
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('l’échec d’un sondage ne rend pas inerte une page déjà chargée', async () => {
    // La garde ne vise que le premier chargement : plus tard, les données sont
    // là et ne mentent pas. Geler la page parce qu'un rafraîchissement d'une
    // seconde a échoué serait une perte de confort sans gain de sûreté.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = serveur({
      scan: { running: true, found: 1, dir: 'a' },
      playlist: [{ path: 'a.mp3', name: 'A' }],
    })
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    s.spy.mockImplementationOnce(async () => new Response('indisponible', { status: 503 }))
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect(w.find('[data-message]').text()).toContain('Erreur : ')
    expect((w.find('[data-clear]').element as HTMLButtonElement).disabled).toBe(false)
  })

  it('vol unique : deux opérations lancées coup sur coup n’en émettent qu’une', async () => {
    // Le SDK sert les requêtes d'admin strictement en série et le cœur
    // abandonne au bout de 5 s : la seconde, mise en file derrière la première,
    // dépasserait le plafond et s'afficherait en « plugin injoignable » pour
    // une action pourtant légitime.
    let debloquer: () => void = () => {}
    const enCours = new Promise<void>((r) => (debloquer = r))
    const s = serveur({ playlist: [{ path: 'a.mp3', name: 'A' }] })
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    s.spy.mockImplementationOnce(async () => {
      await enCours
      return new Response(null, { status: 204 })
    })
    await w.find('[data-clear]').trigger('click')
    await w.find('[data-clear]').trigger('click')
    expect(s.putsDe('clear')).toHaveLength(1)
    debloquer()
    await flushPromises()
    // L'état est rétabli : une nouvelle opération redevient possible.
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsDe('clear')).toHaveLength(2)
  })
})
