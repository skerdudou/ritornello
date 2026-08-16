// `ChoixDossier` est une **feuille** entièrement pilotée par ses propriétés :
// il ne parle ni au serveur ni à une popin. Il se monte donc directement, sans
// le harnais de page — le faire passer par `FilesAdmin` ferait dépendre ces
// cinq assertions du câblage de deux assistants qui, eux, ont leurs propres
// tests.
import { mount } from '@vue/test-utils'
import { createT } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import ChoixDossier from './ChoixDossier.vue'
import type { Exploration } from './donnees'
import { CATALOGUE } from './harnais'

const t = createT(CATALOGUE)

const BASE: Exploration = {
  open: true,
  kind: 'local',
  host: '',
  share: '',
  path: '/media/usb',
  shares: [],
  dirs: ['Albums', 'Live'],
  audioCount: 3,
  busy: false,
  error: null,
}

function monter(surcharges: Partial<Exploration> = {}) {
  return mount(ChoixDossier, {
    props: { exploration: { ...BASE, ...surcharges }, t, fige: false },
  })
}

describe('ChoixDossier', () => {
  it('liste les sous-dossiers et annonce le compte audio', () => {
    // Le compte est ce qui dit qu'on est au bon endroit : sans lui on choisit
    // un dossier en espérant.
    const w = monter()
    expect(w.findAll('[data-choix-dossier]')).toHaveLength(2)
    expect(w.get('[data-audio-count]').text()).toContain('3')
  })

  it('descendre émet le nom du dossier, pas un chemin', async () => {
    // C'est l'appelant qui sait composer le chemin : un chemin local et un
    // chemin SMB ne se composent pas de la même façon.
    const w = monter()
    await w.findAll('[data-choix-dossier]')[1]!.trigger('click')
    expect(w.emitted('descendre')?.[0]).toEqual(['Live'])
  })

  it('un dossier vide le dit au lieu de ne rien afficher', () => {
    // Une liste vide sans phrase se lit comme un chargement qui n'a pas fini.
    const w = monter({ dirs: [] })
    expect(w.find('[data-choix-vide]').exists()).toBe(true)
  })

  it('pendant une attente rien n’est cliquable et l’attente se voit', () => {
    // Sans le disabled, un double clic impatient empilerait deux parcours ;
    // sans la phrase, l'écran figé passerait pour un dossier qui ne répond pas.
    const w = monter({ busy: true })
    expect(w.find('[data-choix-busy]').exists()).toBe(true)
    expect(w.findAll('[data-choix-dossier]')[0]!.attributes('disabled')).toBeDefined()
    expect(w.get('[data-choix-remonter]').attributes('disabled')).toBeDefined()
  })

  it('un refus s’affiche à la place de l’arbre', () => {
    // Afficher un arbre vide sous un message d'erreur laisserait croire que le
    // dossier existe et qu'il est vide.
    const w = monter({ error: 'hôte injoignable' })
    expect(w.get('[data-choix-erreur]').text()).toContain('hôte injoignable')
    expect(w.findAll('[data-choix-dossier]')).toHaveLength(0)
    expect(w.find('[data-choix-vide]').exists()).toBe(false)
  })
})
