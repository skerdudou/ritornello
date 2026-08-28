// Même montage direct que `DeviceDialog`, et pour la même raison :
// `SourcesPane`, qui ouvrira cette popin, n'existe pas encore. Le contenu part
// dans un portail vers `document.body` — `wrapper.find()` ne le voit jamais, et
// sans `attachTo` il n'est même pas rendu.
import { flushPromises, mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { afterEach, describe, expect, it, vi } from 'vitest'
import ShareDialog from './ShareDialog.vue'
import { normalizeData, type Send } from './data'
import {
  CATALOG,
  EXPLORE_CLOSED,
  clickPopover,
  inPopover,
  state,
  cleanupPopovers,
  typeInPopover,
} from './harness'
import type { ServerState } from './harness'

const t = createT(CATALOG)

afterEach(cleanupPopovers)

/** L'état initial se déclare en forme **server** (`snake_case`). */
async function mountDialog(partiel: ServerState = {}, message = '') {
  const data = normalizeData(state({ can_browse_smb: true, ...partiel }))
  const send = vi.fn<Send>().mockResolvedValue(data)
  const w = mount(ShareDialog, {
    props: { data, t, send, fige: false, ouvert: true, message },
    attachTo: document.body,
  })
  // Le portail n'est peuplé qu'au cycle suivant le montage.
  await flushPromises()
  return { w, send }
}

/** Assistant connecté à un hôte, dans l'un ou l'autre de ses deux temps. */
function connecte(surcharges: Record<string, unknown>) {
  return {
    explore: { ...EXPLORE_CLOSED, open: true, kind: 'smb' as const, host: 'nas', ...surcharges },
  }
}

describe('ShareDialog', () => {
  it('se connect envoie l’hôte et les identifiants une seule fois', async () => {
    const { send } = await mountDialog()
    await typeInPopover('[data-host]', '192.168.1.20')
    await typeInPopover('[data-user]', 'steven')
    await typeInPopover('[data-password]', 'secret')
    await clickPopover('[data-connect]')
    expect(send).toHaveBeenCalledWith({
      op: 'smb_connect',
      host: '192.168.1.20',
      user: 'steven',
      password: 'secret',
      domain: '',
    })
    expect(send).toHaveBeenCalledTimes(1)
  })

  it('choose un partage demande sa root', async () => {
    const { send } = await mountDialog(connecte({ shares: ['musique'] }))
    await clickPopover('[data-share]')
    expect(send).toHaveBeenCalledWith({ op: 'smb_browse', share: 'musique', path: '' })
  })

  it('descend garde le partage et compose un path relatif', async () => {
    // Relatif au partage, et non absolu : c'est ce que `smbclient -D` attend,
    // et une barre oblique de tête le ferait repartir de la root du partage.
    const { send } = await mountDialog(
      connecte({ share: 'musique', path: 'Ma Musique', dirs: ['Jazz'] }),
    )
    await clickPopover('[data-choix-dossier]')
    expect(send).toHaveBeenCalledWith({
      op: 'smb_browse',
      share: 'musique',
      path: 'Ma Musique/Jazz',
    })
  })

  it('confirmer déclare la source sans réclamer le mot de passe', async () => {
    // Il vient de servir à se connect : le faire retaper serait absurde, et
    // la page ne l'a de toute façon jamais reçu en retour.
    const { send } = await mountDialog(
      connecte({ share: 'musique', path: 'Ma Musique', shares: ['musique'], dirs: [] }),
    )
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({
      op: 'add_source',
      kind: 'smb',
      path: null,
      host: 'nas',
      share: 'musique',
      subpath: 'Ma Musique',
      user: '',
      domain: '',
      password: '',
      writable: false,
    })
  })

  it('sans smbclient la popin est d’emblée en input manuelle', async () => {
    // Plus de bouton grisé à comprendre : il n'y a rien à parcourir, donc rien
    // à toggle. La raison reste nommée, elle explique pourquoi les champs
    // remplacent l'assistant.
    const { send } = await mountDialog({ can_browse_smb: false })
    expect((inPopover('[data-smb-unavailable]')?.textContent ?? '').length).toBeGreaterThan(0)
    expect(inPopover('[data-manual-share]')).not.toBeNull()
    expect(inPopover('[data-connect]')).toBeNull()
    expect(inPopover('[data-manual]')).toBeNull()
    expect(send).not.toHaveBeenCalled()
  })

  it('avec smbclient la bascule manuelle reste offerte, et l’assistant est le défaut', async () => {
    await mountDialog({ can_browse_smb: true })
    expect(inPopover('[data-manual-share]')).toBeNull()
    expect(inPopover('[data-manual]')).not.toBeNull()
    await clickPopover('[data-manual]')
    expect(inPopover('[data-manual-share]')).not.toBeNull()
  })

  it('le champ domaine dit qu’il est optionnel', async () => {
    // Signalé à l'usage : « domaine » seul ne dit pas à quoi il sert, et se lit
    // comme un champ à remplir. Il ne sert qu'à un compte de domaine Windows.
    await mountDialog({ can_browse_smb: true })
    expect(inPopover('[data-domain]')?.getAttribute('placeholder')).toContain('optionnel')
  })

  it('la input manuelle déclare la source directement', async () => {
    const { send } = await mountDialog({ can_browse_smb: false })
    await typeInPopover('[data-host]', 'nas')
    await typeInPopover('[data-manual-share]', 'musique')
    await typeInPopover('[data-manual-subpath]', 'Albums')
    await typeInPopover('[data-user]', 'steven')
    await typeInPopover('[data-password]', 'secret')
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({
      op: 'add_source',
      kind: 'smb',
      path: null,
      host: 'nas',
      share: 'musique',
      subpath: 'Albums',
      user: 'steven',
      domain: '',
      password: 'secret',
      writable: false,
    })
  })

  it('un sous-path manual laissé vide part à null, jamais à la chaîne vide', async () => {
    // Côté plugin c'est une `Option<String>` : `Some("")` n'est pas « pas de
    // sous-path » mais un sous-path vide, que la validation refuse — le
    // partage serait alors indéclarable sans qu'aucun champ n'ait l'air fautif.
    const { send } = await mountDialog({ can_browse_smb: false })
    await typeInPopover('[data-host]', 'nas')
    await typeInPopover('[data-manual-share]', 'musique')
    await clickPopover('[data-choose]')
    expect(send.mock.calls[0]?.[0]).toMatchObject({ subpath: null })
  })

  it('un refus s’affiche au lieu d’une liste de partages vide', async () => {
    // Une popin muette après un clic sur « Se connect » se lit comme une
    // connexion qui n'a jamais eu lieu.
    await mountDialog(connecte({ error: 'hôte injoignable' }))
    expect(inPopover('[data-partage-erreur]')?.textContent ?? '').toContain('hôte injoignable')
    expect(inPopover('[data-share]')).toBeNull()
  })

  it('le partage reste visible dans le path quand on descend dedans', async () => {
    // Défaut signalé : `explore.path` est relatif au partage, donc le partage
    // choisi n'apparaissait nulle part — il semblait « mangé » dès qu'on y
    // entrait, et rien ne disait dans lequel on se trouvait.
    await mountDialog({
      explore: {
        ...EXPLORE_CLOSED,
        open: true,
        kind: 'smb',
        host: '192.168.1.15',
        share: 'music',
        path: 'Yann Tiersen',
        shares: ['music'],
      },
    })
    expect(inPopover('[data-choix-path]')?.getAttribute('title')).toBe(
      '//192.168.1.15/music/Yann Tiersen',
    )
  })

  it('au sommet d’un partage, goUp ramène à la liste des partages', async () => {
    // Défaut signalé : là, goUp ne faisait rien du tout, et il fallait
    // refermer la popin pour essayer un autre partage.
    const { send } = await mountDialog(connecte({ share: 'music', path: '' }))
    await clickPopover('[data-choix-goUp]')
    expect(send).toHaveBeenCalledWith({ op: 'smb_shares' })
  })

  it('un bouton explicite ramène aussi à la liste des partages', async () => {
    const { send } = await mountDialog(connecte({ share: 'music', path: 'Yann Tiersen' }))
    await clickPopover('[data-aux-partages]')
    expect(send).toHaveBeenCalledWith({ op: 'smb_shares' })
  })

  it('revenir aux partages ne relance aucun appel réseau', async () => {
    // `smb_shares` et non `smb_connect` : les partages sont déjà connus, et
    // refaire l'appel ferait attendre — voire échouer — un simple retour.
    const { send } = await mountDialog(connecte({ share: 'music', path: 'Yann Tiersen' }))
    await clickPopover('[data-aux-partages]')
    expect(send.mock.calls.some((c) => (c[0] as { op: string }).op === 'smb_connect')).toBe(false)
  })

  it('rouvrir la popin ne garde rien de la input précédente', async () => {
    // Le `Dialog` reste monté quand il est fermé : sans remise à zéro, l'hôte et
    // le mot de passe de la fois précédente réapparaissaient — un secret qui n'a
    // rien à faire en mémoire une fois la popin refermée.
    const { w } = await mountDialog()
    await typeInPopover('[data-host]', '192.168.1.15')
    await typeInPopover('[data-password]', 'secret-du-nas')
    await w.setProps({ ouvert: false })
    await w.setProps({ ouvert: true })
    expect((inPopover('[data-host]') as HTMLInputElement).value).toBe('')
    expect((inPopover('[data-password]') as HTMLInputElement).value).toBe('')
  })

  it('affiche le refus du plugin dans la popin, pas seulement sur la page', async () => {
    // Même défaut que pour l'assistant local : le bandeau de la page vit
    // derrière le voile gris de la boîte de dialogue, donc illisible au moment
    // où il compte. Ce path-ci porte les refus de `add_source` — un doublon,
    // par exemple — que `explore.error` ne transporte pas.
    const refus = 'Ce dossier est déjà déclaré comme source.'
    await mountDialog(connecte({}), refus)
    expect(inPopover('[data-dlg-message]')?.textContent).toContain(refus)
  })
})
