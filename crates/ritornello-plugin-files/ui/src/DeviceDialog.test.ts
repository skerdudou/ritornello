// L'assistant se monte **directement**, et non par la page : `SourcesPane`,
// qui l'ouvrira, n'existe pas encore. Le montage direct exige tout de même
// `attachTo: document.body` et `inPopover` — le contenu d'un `Dialog` part dans
// un portail vers `document.body`, donc `wrapper.find()` ne le trouve jamais,
// et sans rattachement il n'est même pas rendu.
import { flushPromises, mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DeviceDialog from './DeviceDialog.vue'
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

// Les portails ne sont pas nettoyés par le démontage du wrapper : sans cela, la
// popin d'un test précédent resterait dans le document et le test suivant
// interrogerait le mauvais panneau.
afterEach(cleanupPopovers)

/**
 * L'état initial se déclare en forme **server** (`snake_case`), comme le
 * plugin le sérialise, puis passe par `normalizeData` : c'est le seul
 * path que la vraie page emprunte, et le seul qui éprouve la normalisation
 * en même temps que le gabarit.
 */
async function mountDialog(partiel: ServerState = {}, message = '') {
  const data = normalizeData(
    state({ volumes: [{ path: '/media/usb', fstype: 'vfat' }], ...partiel }),
  )
  const send = vi.fn<Send>().mockResolvedValue(data)
  const w = mount(DeviceDialog, {
    props: { data, t, send, fige: false, ouvert: true, message },
    attachTo: document.body,
  })
  // Le portail n'est peuplé qu'au cycle suivant le montage : interroger le
  // document tout de suite ne trouve rien, et fait search un défaut dans le
  // gabarit alors que la popin arrive une frame plus tard.
  await flushPromises()
  return { w, send }
}

/** Assistant ouvert sur un volume, donc déjà dans l'arbre. */
function inTree(path: string, dirs: string[]) {
  return { explore: { ...EXPLORE_CLOSED, open: true, kind: 'local' as const, path, dirs } }
}

describe('DeviceDialog', () => {
  it('propose les volumes montés', async () => {
    // Il ouvre sur les volumes et jamais sur `/` : le path absolu d'une clé
    // USB n'est connu de personne, et c'est pourtant ce que l'ancien
    // formulaire demandait de taper.
    const { w } = await mountDialog()
    // La popin vit dans un portail : `w.find` ne la voit JAMAIS.
    expect(w.find('[data-volume]').exists()).toBe(false)
    expect(inPopover('[data-volume]')?.textContent ?? '').toContain('/media/usb')
  })

  it('choose un volume demande son contenu au plugin', async () => {
    const { send } = await mountDialog()
    await clickPopover('[data-volume]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb' })
  })

  it('descend compose le path absolu', async () => {
    // L'arbre n'émet qu'un nom : c'est ici que le path se compose, et un
    // path local se compose avec des barres obliques.
    const { send } = await mountDialog(inTree('/media/usb', ['Albums']))
    await clickPopover('[data-choix-dossier]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('goUp descend d’un cran tant qu’on reste dans le volume', async () => {
    const { send } = await mountDialog(inTree('/media/usb/Albums/Jazz', []))
    await clickPopover('[data-choix-goUp]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('au sommet d’un volume, goUp ramène à la liste des volumes', async () => {
    // Défaut signalé : une fois un volume choisi, on ne pouvait plus en essayer
    // un autre. Remonter emmenait dans `/media` puis `/` — on sortait du volume
    // sans jamais retrouver la liste, et il fallait close la popin.
    const { send } = await mountDialog(inTree('/media/usb', []))
    await clickPopover('[data-choix-goUp]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('un bouton explicite ramène aussi à la liste des volumes', async () => {
    // Le retour ne doit pas se deviner : goUp jusqu'au sommet pour espérer
    // retomber sur la liste n'est pas une manœuvre qu'on invente.
    const { send } = await mountDialog(inTree('/media/usb/Albums', []))
    await clickPopover('[data-aux-volumes]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('un path hors de tout volume connu ramène à la liste', async () => {
    // Plutôt que de goUp à l'aveugle : si on ne sait pas situer le path
    // dans un volume déclaré, la liste est le seul repère sûr.
    const { send } = await mountDialog(inTree('/ailleurs', []))
    await clickPopover('[data-choix-goUp]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
  })

  it('un path saisi à la main ouvre ce dossier au lieu de le déclarer', async () => {
    // Naviguer et non déclarer, à dessein : on garde la vérification qui fait
    // l'intérêt de la popin — le contenu du dossier et son compte de fichiers
    // audio — avant de valider quoi que ce soit.
    const { send } = await mountDialog()
    await typeInPopover('[data-manual-path]', '  /srv/musique  ')
    await clickPopover('[data-manual-go]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_local', path: '/srv/musique' })
  })

  it('la input manuelle reste offerte une fois dans l’arborescence', async () => {
    // C'est même là qu'elle sert le plus : on s'est trompé de branche et on
    // veut sauter ailleurs sans goUp clic par clic.
    await mountDialog(inTree('/media/usb/Albums', []))
    expect(inPopover('[data-manual-path]')).not.toBeNull()
  })

  it('une input vide ne déclenche rien', async () => {
    // Sinon le bouton enverrait un `explore_local` sur la chaîne vide, que le
    // plugin refuserait — un refus provoqué par l'IHM elle-même.
    const { send } = await mountDialog()
    await typeInPopover('[data-manual-path]', '   ')
    expect(inPopover('[data-manual-go]')?.hasAttribute('disabled')).toBe(true)
    expect(send).not.toHaveBeenCalled()
  })

  it('rouvrir la popin ne garde rien de la input précédente', async () => {
    // Le `Dialog` reste monté quand il est fermé : sans remise à zéro, le path
    // tapé la fois précédente réapparaissait, comme si on n'avait jamais fermé.
    const { w } = await mountDialog()
    await typeInPopover('[data-manual-path]', '/srv/musique')
    await w.setProps({ ouvert: false })
    await w.setProps({ ouvert: true })
    expect((inPopover('[data-manual-path]') as HTMLInputElement).value).toBe('')
  })

  it('affiche le refus du plugin dans la popin, pas seulement sur la page', async () => {
    // Défaut signalé : le message atterrissait sur la page principale, derrière
    // le voile gris de la boîte de dialogue — donc illisible au moment précis
    // où il compte, quand on vient de choose un dossier interdit.
    const refus = 'Ce path n’est pas parcourable : /root/prive'
    const { w } = await mountDialog(inTree('/media/usb', []), refus)
    expect(inPopover('[data-dlg-message]')?.textContent).toContain(refus)
    void w
  })

  it('confirmer déclare la source avec le path courant', async () => {
    const { send } = await mountDialog(inTree('/media/usb/Albums', []))
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({
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
    const { w, send } = await mountDialog(inTree('/media/usb/Albums', []))
    await clickPopover('[data-choose]')
    expect(send).toHaveBeenCalledWith({ op: 'explore_close' })
    expect(w.emitted('close')).toHaveLength(1)
  })

  it('tant qu’aucun volume n’est choisi, confirmer est hors de portée', async () => {
    // Déclarer une source sans path serait un refus du plugin devant un
    // bouton qui avait l'air prêt.
    const { send } = await mountDialog()
    expect(inPopover('[data-choose]')?.getAttribute('disabled')).not.toBeNull()
    expect(send).not.toHaveBeenCalled()
  })

  it('sans volume la popin le dit au lieu d’offrir une liste vide', async () => {
    const { w } = await mountDialog({ volumes: [] })
    expect(inPopover('[data-no-volumes]')).not.toBeNull()
    expect(inPopover('[data-volume]')).toBeNull()
    expect(w.find('[data-no-volumes]').exists()).toBe(false)
  })
})
