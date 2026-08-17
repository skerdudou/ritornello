// L'assistant se monte **directement**, et non par la page : `VoletSources`,
// qui l'ouvrira, n'existe pas encore. Le montage direct exige tout de même
// `attachTo: document.body` et `dansPopin` — le contenu d'un `Dialog` part dans
// un portail vers `document.body`, donc `wrapper.find()` ne le trouve jamais,
// et sans rattachement il n'est même pas rendu.
import { flushPromises, mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DialogueAppareil from './DialogueAppareil.vue'
import { normaliserDonnees, type Envoyer } from './donnees'
import { CATALOGUE, EXPLORE_FERME, cliquerPopin, dansPopin, etat, nettoyerPopins } from './harnais'
import type { EtatServeur } from './harnais'

const t = createT(CATALOGUE)

// Les portails ne sont pas nettoyés par le démontage du wrapper : sans cela, la
// popin d'un test précédent resterait dans le document et le test suivant
// interrogerait le mauvais panneau.
afterEach(nettoyerPopins)

/**
 * L'état initial se déclare en forme **serveur** (`snake_case`), comme le
 * plugin le sérialise, puis passe par `normaliserDonnees` : c'est le seul
 * chemin que la vraie page emprunte, et le seul qui éprouve la normalisation
 * en même temps que le gabarit.
 */
async function monter(partiel: EtatServeur = {}, message = '') {
  const donnees = normaliserDonnees(
    etat({ volumes: [{ path: '/media/usb', fstype: 'vfat' }], ...partiel }),
  )
  const envoyer = vi.fn<Envoyer>().mockResolvedValue(donnees)
  const w = mount(DialogueAppareil, {
    props: { donnees, t, envoyer, fige: false, ouvert: true, message },
    attachTo: document.body,
  })
  // Le portail n'est peuplé qu'au cycle suivant le montage : interroger le
  // document tout de suite ne trouve rien, et fait chercher un défaut dans le
  // gabarit alors que la popin arrive une frame plus tard.
  await flushPromises()
  return { w, envoyer }
}

/** Assistant ouvert sur un volume, donc déjà dans l'arbre. */
function dansLArbre(path: string, dirs: string[]) {
  return { explore: { ...EXPLORE_FERME, open: true, kind: 'local' as const, path, dirs } }
}

describe('DialogueAppareil', () => {
  it('propose les volumes montés', async () => {
    // Il ouvre sur les volumes et jamais sur `/` : le chemin absolu d'une clé
    // USB n'est connu de personne, et c'est pourtant ce que l'ancien
    // formulaire demandait de taper.
    const { w } = await monter()
    // La popin vit dans un portail : `w.find` ne la voit JAMAIS.
    expect(w.find('[data-volume]').exists()).toBe(false)
    expect(dansPopin('[data-volume]')?.textContent ?? '').toContain('/media/usb')
  })

  it('choisir un volume demande son contenu au plugin', async () => {
    const { envoyer } = await monter()
    await cliquerPopin('[data-volume]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb' })
  })

  it('descendre compose le chemin absolu', async () => {
    // L'arbre n'émet qu'un nom : c'est ici que le chemin se compose, et un
    // chemin local se compose avec des barres obliques.
    const { envoyer } = await monter(dansLArbre('/media/usb', ['Albums']))
    await cliquerPopin('[data-choix-dossier]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('remonter descend d’un cran tant qu’on reste dans le volume', async () => {
    const { envoyer } = await monter(dansLArbre('/media/usb/Albums/Jazz', []))
    await cliquerPopin('[data-choix-remonter]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('au sommet d’un volume, remonter ramène à la liste des volumes', async () => {
    // Défaut signalé : une fois un volume choisi, on ne pouvait plus en essayer
    // un autre. Remonter emmenait dans `/media` puis `/` — on sortait du volume
    // sans jamais retrouver la liste, et il fallait fermer la popin.
    const { envoyer } = await monter(dansLArbre('/media/usb', []))
    await cliquerPopin('[data-choix-remonter]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('un bouton explicite ramène aussi à la liste des volumes', async () => {
    // Le retour ne doit pas se deviner : remonter jusqu'au sommet pour espérer
    // retomber sur la liste n'est pas une manœuvre qu'on invente.
    const { envoyer } = await monter(dansLArbre('/media/usb/Albums', []))
    await cliquerPopin('[data-aux-volumes]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('un chemin hors de tout volume connu ramène à la liste', async () => {
    // Plutôt que de remonter à l'aveugle : si on ne sait pas situer le chemin
    // dans un volume déclaré, la liste est le seul repère sûr.
    const { envoyer } = await monter(dansLArbre('/ailleurs', []))
    await cliquerPopin('[data-choix-remonter]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('affiche le refus du plugin dans la popin, pas seulement sur la page', async () => {
    // Défaut signalé : le message atterrissait sur la page principale, derrière
    // le voile gris de la boîte de dialogue — donc illisible au moment précis
    // où il compte, quand on vient de choisir un dossier interdit.
    const refus = 'Ce chemin n’est pas parcourable : /root/prive'
    const { w } = await monter(dansLArbre('/media/usb', []), refus)
    expect(dansPopin('[data-dlg-message]')?.textContent).toContain(refus)
    void w
  })

  it('confirmer déclare la source avec le chemin courant', async () => {
    const { envoyer } = await monter(dansLArbre('/media/usb/Albums', []))
    await cliquerPopin('[data-choisir]')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'add_source',
      kind: 'local',
      path: '/media/usb/Albums',
      host: '',
      share: '',
      subpath: null,
      user: '',
      domain: '',
      password: '',
      writable: false,
    })
  })

  it('confirmer referme l’assistant côté plugin', async () => {
    // Sans `explore_close`, l'état d'assistant resterait ouvert côté plugin :
    // la popin se rouvrirait toute seule au prochain chargement de la page.
    const { w, envoyer } = await monter(dansLArbre('/media/usb/Albums', []))
    await cliquerPopin('[data-choisir]')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_close' })
    expect(w.emitted('fermer')).toHaveLength(1)
  })

  it('tant qu’aucun volume n’est choisi, confirmer est hors de portée', async () => {
    // Déclarer une source sans chemin serait un refus du plugin devant un
    // bouton qui avait l'air prêt.
    const { envoyer } = await monter()
    expect(dansPopin('[data-choisir]')?.getAttribute('disabled')).not.toBeNull()
    expect(envoyer).not.toHaveBeenCalled()
  })

  it('sans volume la popin le dit au lieu d’offrir une liste vide', async () => {
    const { w } = await monter({ volumes: [] })
    expect(dansPopin('[data-no-volumes]')).not.toBeNull()
    expect(dansPopin('[data-volume]')).toBeNull()
    expect(w.find('[data-no-volumes]').exists()).toBe(false)
  })
})
