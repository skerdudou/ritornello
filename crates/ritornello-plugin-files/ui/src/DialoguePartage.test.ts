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
async function monter(partiel: EtatServeur = {}) {
  const donnees = normaliserDonnees(etat({ can_browse_smb: true, ...partiel }))
  const envoyer = vi.fn<Envoyer>().mockResolvedValue(donnees)
  const w = mount(DialoguePartage, {
    props: { donnees, t, envoyer, fige: false, ouvert: true },
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

  it('sans smbclient l’assistant est grisé et la raison est nommée', async () => {
    // Comme l'onglet Système grise le redémarrage : jamais de plantage, jamais
    // un bouton qui échoue sans dire pourquoi.
    const { envoyer } = await monter({ can_browse_smb: false })
    expect((dansPopin('[data-smb-unavailable]')?.textContent ?? '').length).toBeGreaterThan(0)
    expect(dansPopin('[data-connect]')?.getAttribute('disabled')).not.toBeNull()
    expect(envoyer).not.toHaveBeenCalled()
  })

  it('le repli manuel reste offert sans smbclient', async () => {
    // Sans lui, ce chantier RETIRERAIT une capacité qui existe aujourd'hui.
    await monter({ can_browse_smb: false })
    expect(dansPopin('[data-manual-share]')).toBeNull()
    await cliquerPopin('[data-manuel]')
    expect(dansPopin('[data-manual-share]')).not.toBeNull()
  })

  it('la saisie manuelle déclare la source directement', async () => {
    const { envoyer } = await monter({ can_browse_smb: false })
    await cliquerPopin('[data-manuel]')
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
    await cliquerPopin('[data-manuel]')
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
})
