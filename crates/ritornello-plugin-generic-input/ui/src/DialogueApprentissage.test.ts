// La popin d'apprentissage part dans un portail vers `document.body` (comme
// tout `Dialog` du kit) : `wrapper.find()` ne la voit jamais, il faut monter
// avec `attachTo: document.body` et chercher dans le document. Les portails
// ne sont pas nettoyés par le démontage du wrapper, d'où le `beforeEach`.
import { createT, Dialog } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it } from 'vitest'
import DialogueApprentissage from './DialogueApprentissage.vue'

const CATALOGUE: Record<string, string> = {
  dlg_learn_title: 'Apprentissage d’une touche',
  dlg_learn_desc: 'Appuyez sur une touche du périphérique « {device} »…',
  learn_append_label: 'Ajouter aux codes existants au lieu de les remplacer',
  learn_countdown: 'Il reste {s} s',
  btn_cancel: 'Annuler',
}
// Le vrai traducteur du kit, et non un bouchon qui rendrait la valeur brute :
// c'est lui qui interpole les jetons `{nom}`, et la popin doit lui passer ses
// paramètres plutôt que de substituer elle-même.
const t = createT(CATALOGUE)

interface Props {
  ouvert: boolean
  t: typeof t
  action: string
  device: string
  ajouter: boolean
  secondes: number
}

beforeEach(() => {
  document.body.innerHTML = ''
})

function monter(props: Partial<Props> = {}) {
  return mount(DialogueApprentissage, {
    props: {
      ouvert: true,
      t,
      action: 'Muet',
      device: 'mce',
      ajouter: false,
      secondes: 30,
      ...props,
    },
    attachTo: document.body,
  })
}

function dansPopin(selecteur: string) {
  return document.body.querySelector(selecteur)
}

describe('DialogueApprentissage', () => {
  it('fermée, elle ne pose rien dans le document', async () => {
    monter({ ouvert: false })
    await flushPromises()
    expect(dansPopin('[data-dlg-learn]')).toBeNull()
  })

  it('ouverte, le titre porte l’action et la description nomme le périphérique', async () => {
    monter({ action: 'Muet', device: 'mce' })
    await flushPromises()
    const popin = dansPopin('[data-dlg-learn]')
    expect(popin).not.toBeNull()
    expect(popin!.textContent).toContain('Muet')
    expect(popin!.textContent).toContain('mce')
    // Le jeton n'a pas fuité tel quel : la description l'a bien remplacé.
    expect(popin!.textContent).not.toContain('{device}')
  })

  it('sans action à nommer, le titre ne traîne pas de tiret', async () => {
    // La page vide `action` dès le geste de fermeture, alors que reka-ui garde
    // le contenu monté le temps du fondu de sortie : pendant ces 200 ms, le
    // titre ne doit pas se lire « Apprentissage d’une touche — ».
    monter({ action: '' })
    await flushPromises()
    expect(dansPopin('[data-slot="dialog-title"]')!.textContent).toBe('Apprentissage d’une touche')
  })

  it('le bouton Annuler émet un et un seul « annuler »', async () => {
    const w = monter()
    await flushPromises()
    ;(dansPopin('[data-learn-cancel]') as HTMLButtonElement).click()
    await flushPromises()
    expect(w.emitted('annuler')).toHaveLength(1)
  })

  it('la fermeture par `update:open` émet un et un seul « annuler »', async () => {
    // Échap, le clic sur le voile et la croix du kit passent tous par ce seul
    // chemin ; `[data-learn-cancel]`, qui émet `annuler` en direct, ne
    // l'exerce jamais.
    const w = monter()
    await flushPromises()
    w.findComponent(Dialog).vm.$emit('update:open', false)
    await flushPromises()
    expect(w.emitted('annuler')).toHaveLength(1)
  })

  it('la croix posée par `DialogContent` annule elle aussi', async () => {
    // Le kit rend une `DialogClose` dès que `showCloseButton` n'est pas nié —
    // et il vaut `true` par défaut. Troisième déclencheur d'annulation, celui
    // qu'aucune ligne du template de cette popin ne laisse voir.
    const w = monter()
    await flushPromises()
    const croix = dansPopin('[data-slot="dialog-close"]') as HTMLButtonElement | null
    expect(croix).not.toBeNull()
    croix!.click()
    await flushPromises()
    expect(w.emitted('annuler')).toHaveLength(1)
  })

  it('cocher la case émet update:ajouter à true, sans état propre', async () => {
    const w = monter({ ajouter: false })
    await flushPromises()
    const case_ = dansPopin('[data-learn-append]') as HTMLInputElement
    expect(case_.checked).toBe(false)
    case_.checked = true
    case_.dispatchEvent(new Event('change'))
    await flushPromises()
    expect(w.emitted('update:ajouter')).toEqual([[true]])
  })

  it('ajouter: true affiche la case cochée', async () => {
    monter({ ajouter: true })
    await flushPromises()
    expect((dansPopin('[data-learn-append]') as HTMLInputElement).checked).toBe(true)
  })

  it('affiche le temps qu’il reste pour appuyer', async () => {
    // Sans compte à rebours, la popin se referme d'elle-même au bout de 30 s
    // sans que rien n'ait laissé prévoir l'échéance : on croit l'appareil muet
    // alors qu'on a simplement mis trop de temps à trouver la touche.
    monter({ secondes: 27 })
    await flushPromises()
    expect(dansPopin('[data-learn-countdown]')?.textContent).toContain('27')
  })

  it('ne montre pas de compte à rebours une fois l’échéance atteinte', async () => {
    // À zéro la page a déjà arrêté l'apprentissage : afficher « il reste 0 s »
    // pendant la fermeture donnerait un décompte qui ment.
    monter({ secondes: 0 })
    await flushPromises()
    expect(dansPopin('[data-learn-countdown]')).toBeNull()
  })
})
