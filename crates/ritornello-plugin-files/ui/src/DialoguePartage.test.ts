// Même montage direct que `DialogueAppareil`, et pour la même raison :
// `VoletSources`, qui ouvrira cette popin, n'existe pas encore. Le contenu part
// dans un portail vers `document.body` — `wrapper.find()` ne le voit jamais, et
// sans `attachTo` il n'est même pas rendu.
import { flushPromises, mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DialoguePartage from './DialoguePartage.vue'
import { normaliserDonnees, type Envoyer } from './donnees'
import {
  CATALOGUE,
  EXPLORE_FERME,
  cliquerPopin,
  dansPopin,
  etat,
  nettoyerPopins,
  saisirPopin,
} from './harnais'
import type { EtatServeur } from './harnais'

const t = createT(CATALOGUE)

afterEach(nettoyerPopins)

/** L'état initial se déclare en forme **serveur** (`snake_case`). */
async function monter(partiel: EtatServeur = {}, message = '') {
  const donnees = normaliserDonnees(etat({ can_browse_smb: true, ...partiel }))
  const envoyer = vi.fn<Envoyer>().mockResolvedValue(donnees)
  const w = mount(DialoguePartage, {
    props: { donnees, t, envoyer, fige: false, ouvert: true, message },
    attachTo: document.body,
  })
  // Le portail n'est peuplé qu'au cycle suivant le montage.
  await flushPromises()
  return { w, envoyer }
}

/** Assistant connecté à un hôte, dans l'un ou l'autre de ses deux temps. */
function connecte(surcharges: Record<string, unknown>) {
  return {
    explore: { ...EXPLORE_FERME, open: true, kind: 'smb' as const, host: 'nas', ...surcharges },
  }
}

describe('DialoguePartage', () => {
  it('se connecter envoie l’hôte et les identifiants une seule fois', async () => {
    const { envoyer } = await monter()
    await saisirPopin('[data-host]', '192.168.1.20')
    await saisirPopin('[data-user]', 'steven')
    await saisirPopin('[data-password]', 'secret')
    await cliquerPopin('[data-connect]')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'smb_connect',
      host: '192.168.1.20',
      user: 'steven',
      password: 'secret',
      domain: '',
    })
    expect(envoyer).toHaveBeenCalledTimes(1)
  })

  it('choisir un partage demande sa racine', async () => {
    const { envoyer } = await monter(connecte({ shares: ['musique'] }))
    await cliquerPopin('[data-share]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'smb_browse', share: 'musique', path: '' })
  })

  it('descendre garde le partage et compose un chemin relatif', async () => {
    // Relatif au partage, et non absolu : c'est ce que `smbclient -D` attend,
    // et une barre oblique de tête le ferait repartir de la racine du partage.
    const { envoyer } = await monter(
      connecte({ share: 'musique', path: 'Ma Musique', dirs: ['Jazz'] }),
    )
    await cliquerPopin('[data-choix-dossier]')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'smb_browse',
      share: 'musique',
      path: 'Ma Musique/Jazz',
    })
  })

  it('confirmer déclare la source sans réclamer le mot de passe', async () => {
    // Il vient de servir à se connecter : le faire retaper serait absurde, et
    // la page ne l'a de toute façon jamais reçu en retour.
    const { envoyer } = await monter(
      connecte({ share: 'musique', path: 'Ma Musique', shares: ['musique'], dirs: [] }),
    )
    await cliquerPopin('[data-choisir]')
    expect(envoyer).toHaveBeenCalledWith({
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

  it('sans smbclient la popin est d’emblée en saisie manuelle', async () => {
    // Plus de bouton grisé à comprendre : il n'y a rien à parcourir, donc rien
    // à basculer. La raison reste nommée, elle explique pourquoi les champs
    // remplacent l'assistant.
    const { envoyer } = await monter({ can_browse_smb: false })
    expect((dansPopin('[data-smb-unavailable]')?.textContent ?? '').length).toBeGreaterThan(0)
    expect(dansPopin('[data-manual-share]')).not.toBeNull()
    expect(dansPopin('[data-connect]')).toBeNull()
    expect(dansPopin('[data-manuel]')).toBeNull()
    expect(envoyer).not.toHaveBeenCalled()
  })

  it('avec smbclient la bascule manuelle reste offerte, et l’assistant est le défaut', async () => {
    await monter({ can_browse_smb: true })
    expect(dansPopin('[data-manual-share]')).toBeNull()
    expect(dansPopin('[data-manuel]')).not.toBeNull()
    await cliquerPopin('[data-manuel]')
    expect(dansPopin('[data-manual-share]')).not.toBeNull()
  })

  it('le champ domaine dit qu’il est optionnel', async () => {
    // Signalé à l'usage : « domaine » seul ne dit pas à quoi il sert, et se lit
    // comme un champ à remplir. Il ne sert qu'à un compte de domaine Windows.
    await monter({ can_browse_smb: true })
    expect(dansPopin('[data-domain]')?.getAttribute('placeholder')).toContain('optionnel')
  })

  it('la saisie manuelle déclare la source directement', async () => {
    const { envoyer } = await monter({ can_browse_smb: false })
    await saisirPopin('[data-host]', 'nas')
    await saisirPopin('[data-manual-share]', 'musique')
    await saisirPopin('[data-manual-subpath]', 'Albums')
    await saisirPopin('[data-user]', 'steven')
    await saisirPopin('[data-password]', 'secret')
    await cliquerPopin('[data-choisir]')
    expect(envoyer).toHaveBeenCalledWith({
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

  it('un sous-chemin manuel laissé vide part à null, jamais à la chaîne vide', async () => {
    // Côté plugin c'est une `Option<String>` : `Some("")` n'est pas « pas de
    // sous-chemin » mais un sous-chemin vide, que la validation refuse — le
    // partage serait alors indéclarable sans qu'aucun champ n'ait l'air fautif.
    const { envoyer } = await monter({ can_browse_smb: false })
    await saisirPopin('[data-host]', 'nas')
    await saisirPopin('[data-manual-share]', 'musique')
    await cliquerPopin('[data-choisir]')
    expect(envoyer.mock.calls[0]?.[0]).toMatchObject({ subpath: null })
  })

  it('un refus s’affiche au lieu d’une liste de partages vide', async () => {
    // Une popin muette après un clic sur « Se connecter » se lit comme une
    // connexion qui n'a jamais eu lieu.
    await monter(connecte({ error: 'hôte injoignable' }))
    expect(dansPopin('[data-partage-erreur]')?.textContent ?? '').toContain('hôte injoignable')
    expect(dansPopin('[data-share]')).toBeNull()
  })

  it('le partage reste visible dans le chemin quand on descend dedans', async () => {
    // Défaut signalé : `explore.path` est relatif au partage, donc le partage
    // choisi n'apparaissait nulle part — il semblait « mangé » dès qu'on y
    // entrait, et rien ne disait dans lequel on se trouvait.
    await monter({
      explore: {
        ...EXPLORE_FERME,
        open: true,
        kind: 'smb',
        host: '192.168.1.15',
        share: 'music',
        path: 'Yann Tiersen',
        shares: ['music'],
      },
    })
    expect(dansPopin('[data-choix-chemin]')?.getAttribute('title')).toBe(
      '//192.168.1.15/music/Yann Tiersen',
    )
  })

  it('au sommet d’un partage, remonter ramène à la liste des partages', async () => {
    // Défaut signalé : là, remonter ne faisait rien du tout, et il fallait
    // refermer la popin pour essayer un autre partage.
    const { envoyer } = await monter(connecte({ share: 'music', path: '' }))
    await cliquerPopin('[data-choix-remonter]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'smb_shares' })
  })

  it('un bouton explicite ramène aussi à la liste des partages', async () => {
    const { envoyer } = await monter(connecte({ share: 'music', path: 'Yann Tiersen' }))
    await cliquerPopin('[data-aux-partages]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'smb_shares' })
  })

  it('revenir aux partages ne relance aucun appel réseau', async () => {
    // `smb_shares` et non `smb_connect` : les partages sont déjà connus, et
    // refaire l'appel ferait attendre — voire échouer — un simple retour.
    const { envoyer } = await monter(connecte({ share: 'music', path: 'Yann Tiersen' }))
    await cliquerPopin('[data-aux-partages]')
    expect(envoyer.mock.calls.some((c) => (c[0] as { op: string }).op === 'smb_connect')).toBe(false)
  })

  it('rouvrir la popin ne garde rien de la saisie précédente', async () => {
    // Le `Dialog` reste monté quand il est fermé : sans remise à zéro, l'hôte et
    // le mot de passe de la fois précédente réapparaissaient — un secret qui n'a
    // rien à faire en mémoire une fois la popin refermée.
    const { w } = await monter()
    await saisirPopin('[data-host]', '192.168.1.15')
    await saisirPopin('[data-password]', 'secret-du-nas')
    await w.setProps({ ouvert: false })
    await w.setProps({ ouvert: true })
    expect((dansPopin('[data-host]') as HTMLInputElement).value).toBe('')
    expect((dansPopin('[data-password]') as HTMLInputElement).value).toBe('')
  })

  it('affiche le refus du plugin dans la popin, pas seulement sur la page', async () => {
    // Même défaut que pour l'assistant local : le bandeau de la page vit
    // derrière le voile gris de la boîte de dialogue, donc illisible au moment
    // où il compte. Ce chemin-ci porte les refus de `add_source` — un doublon,
    // par exemple — que `explore.error` ne transporte pas.
    const refus = 'Ce dossier est déjà déclaré comme source.'
    await monter(connecte({}), refus)
    expect(dansPopin('[data-dlg-message]')?.textContent).toContain(refus)
  })
})
