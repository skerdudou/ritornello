import { flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { monter } from './harnais'

const TROIS = [
  { path: 'Albums/Jazz/01.mp3', name: 'Piste 1', duration_s: 245, missing: false },
  { path: 'Albums/Jazz/02.mp3', name: 'Piste 2', duration_s: 0, missing: true },
  { path: 'Albums/Jazz/03.mp3', name: 'Piste 3', duration_s: 3725, missing: false },
]

describe('volet de la liste en cours', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('marque une piste introuvable sans jamais la masquer', async () => {
    // Régression encodée : une liste qui rétrécit toute seule est un défaut
    // qu'on met des mois à attribuer. Un partage démonté, lui, se diagnostique
    // en une seconde tant que les pistes restent affichées, signalées.
    const { w } = await monter({ playlist: TROIS })
    expect(w.findAll('[data-track-row]')).toHaveLength(3)
    expect(w.findAll('[data-track-missing]')).toHaveLength(1)
    expect(w.findAll('[data-track-name]')[1]!.text()).toBe('Piste 2')
    // Le chemin complet est dans l'infobulle : c'est lui qui dit *quel*
    // fichier manque, le nom ne suffit pas à le retrouver sur le partage.
    expect(w.findAll('[data-track-missing]')[0]!.attributes('title')).toBe('Albums/Jazz/02.mp3')
  })

  it('rend les durées, tiret compris pour une durée inconnue', async () => {
    const { w } = await monter({ playlist: TROIS })
    const texte = w.find('[data-volet-liste]').text()
    expect(texte).toContain('4:05')
    expect(texte).toContain('1:02:05')
    expect(texte).toContain('—')
  })

  it('réordonne, retire et vide par indices absolus', async () => {
    const { w, s } = await monter({ playlist: TROIS })
    await w.findAll('[data-track-down]')[0]!.trigger('click')
    await flushPromises()
    expect(s.putsDe('move')).toEqual([{ op: 'move', from: 0, to: 1 }])

    await w.findAll('[data-track-remove]')[2]!.trigger('click')
    await flushPromises()
    expect(s.putsDe('remove')).toEqual([{ op: 'remove', index: 2 }])

    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsDe('clear')).toEqual([{ op: 'clear' }])
  })

  it('borne les flèches aux extrémités de la liste', async () => {
    const { w } = await monter({ playlist: TROIS })
    expect((w.findAll('[data-track-up]')[0]!.element as HTMLButtonElement).disabled).toBe(true)
    expect((w.findAll('[data-track-down]')[2]!.element as HTMLButtonElement).disabled).toBe(true)
  })

  it('réordonne au glisser-déposer, comme la grille des stations', async () => {
    const { w, s } = await monter({ playlist: TROIS })
    const lignes = w.findAll('[data-track-row]')
    await lignes[2]!.trigger('dragstart')
    await lignes[0]!.trigger('drop')
    await flushPromises()
    expect(s.putsDe('move')).toEqual([{ op: 'move', from: 2, to: 0 }])
  })

  it('déposer une piste sur elle-même ne demande rien', async () => {
    // Sinon le moindre clic un peu appuyé sur une ligne déclencherait un
    // déplacement sans effet, et une relecture complète de l'état avec.
    const { w, s } = await monter({ playlist: TROIS })
    const lignes = w.findAll('[data-track-row]')
    await lignes[1]!.trigger('dragstart')
    await lignes[1]!.trigger('drop')
    await flushPromises()
    expect(s.putsDe('move')).toEqual([])
  })

  it('déposer sans avoir rien pris ne demande rien', async () => {
    const { w, s } = await monter({ playlist: TROIS })
    await w.findAll('[data-track-row]')[0]!.trigger('drop')
    await flushPromises()
    expect(s.putsDe('move')).toEqual([])
  })

  it('les rangs envoyés au plugin sont absolus, pas ceux de la page', async () => {
    // Au-delà de deux cents pistes la liste est paginée : confondre le rang
    // affiché avec l'index réel déplacerait une tout autre piste.
    const longue = Array.from({ length: 250 }, (_, i) => ({
      path: `/m/${i}.mp3`,
      name: `${i}`,
      duration_s: 0,
      missing: false,
    }))
    const { w, s } = await monter({ playlist: longue })
    await w.find('[data-page-next]').trigger('click')
    await flushPromises()
    const lignes = w.findAll('[data-track-row]')
    await lignes[1]!.trigger('dragstart')
    await lignes[0]!.trigger('drop')
    await flushPromises()
    expect(s.putsDe('move')).toEqual([{ op: 'move', from: 101, to: 100 }])
  })

  it('affiche ce qu’un m3u chargé n’a pas su retrouver', async () => {
    // Sans cet encart, la liste chargée est simplement plus courte que le
    // fichier, sans que rien ne le dise.
    const { w } = await monter({
      playlist: TROIS,
      unresolved: ['Albums/Rock/perdu.mp3', 'Albums/Rock/aussi.mp3'],
    })
    const encart = w.find('[data-unresolved]')
    expect(encart.text()).toContain('2 entrées non retrouvées')
    expect(encart.findAll('[data-unresolved-row]').map((r) => r.text())).toEqual([
      'Albums/Rock/perdu.mp3',
      'Albums/Rock/aussi.mp3',
    ])
  })

  it('n’affiche pas d’encart quand tout a été résolu', async () => {
    const { w } = await monter({ playlist: TROIS })
    expect(w.find('[data-unresolved]').exists()).toBe(false)
  })

  it('pagine au-delà de deux cents pistes, sans en perdre aucune', async () => {
    // Rendre plusieurs milliers de lignes d'un coup fige l'onglet plusieurs
    // secondes sur le navigateur d'un Raspberry Pi.
    const longue = Array.from({ length: 250 }, (_, i) => ({
      path: `p/${i}.mp3`,
      name: `Piste ${i}`,
      duration_s: 100,
      missing: false,
    }))
    const { w } = await monter({ playlist: longue })
    expect(w.findAll('[data-track-row]')).toHaveLength(100)
    expect(w.find('[data-page-label]').text()).toBe('1–100 sur 250')
    expect(w.findAll('[data-track-num]')[0]!.text()).toBe('1')

    await w.find('[data-page-next]').trigger('click')
    await w.find('[data-page-next]').trigger('click')
    // Dernière page : les cinquante restantes, numérotées depuis leur vrai rang.
    expect(w.findAll('[data-track-row]')).toHaveLength(50)
    expect(w.find('[data-page-label]').text()).toBe('201–250 sur 250')
    expect(w.findAll('[data-track-num]')[0]!.text()).toBe('201')
    expect((w.find('[data-page-next]').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('ouvre la liste paginée sur la page de la piste en cours', async () => {
    // Arriver sur la page 1 d'une liste de mille titres alors que le lecteur en
    // est au 350e n'aide personne.
    const longue = Array.from({ length: 1000 }, (_, i) => ({
      path: `p/${i}.mp3`,
      name: `Piste ${i}`,
      duration_s: 100,
      missing: false,
    }))
    const { w } = await monter({ playlist: longue, index: 349 })
    expect(w.find('[data-page-label]').text()).toBe('301–400 sur 1000')
  })

  it('ne pagine pas une liste courte', async () => {
    const { w } = await monter({ playlist: TROIS })
    expect(w.find('[data-page-label]').exists()).toBe(false)
  })

  it('enregistre la liste sous un nom et une destination', async () => {
    const { w, s } = await monter({
      playlist: TROIS,
      roots: [
        { name: 'nas', kind: 'smb', host: 'h', share: 's', writable: true },
        { name: 'lecture-seule', kind: 'smb', host: 'h', share: 's', writable: false },
      ],
    })
    // Seules les racines **inscriptibles** sont proposées : offrir un partage
    // monté en lecture seule ne produirait qu'un refus du plugin.
    const options = w.findAll('[data-playlist-where] option').map((o) => o.attributes('value'))
    expect(options).toEqual(['internal', 'nas'])

    await w.find('[data-playlist-name]').setValue('Jazz')
    await w.find('[data-playlist-where]').setValue('nas')
    await w.find('[data-save-playlist]').trigger('click')
    await flushPromises()
    expect(s.putsDe('save_playlist')).toEqual([
      { op: 'save_playlist', name: 'Jazz', where: 'nas' },
    ])
  })

  it('n’enregistre rien sans nom de liste', async () => {
    const { w, s } = await monter({ playlist: TROIS })
    await w.find('[data-playlist-name]').setValue('   ')
    await w.find('[data-save-playlist]').trigger('click')
    await flushPromises()
    expect(s.putsDe('save_playlist')).toHaveLength(0)
  })

  it('charge une liste enregistrée depuis son emplacement d’origine', async () => {
    // Le couple nom + emplacement est ce qui identifie une liste : deux
    // « Jazz » peuvent coexister, l'un interne, l'autre sur le partage.
    const { w, s } = await monter({
      saved: [
        { name: 'Jazz', where: 'internal' },
        { name: 'Jazz', where: 'nas' },
      ],
    })
    expect(w.findAll('[data-saved-pick] option').map((o) => o.text().trim())).toEqual([
      'Jazz — stockage interne',
      'Jazz — nas',
    ])
    await w.find('[data-saved-pick]').setValue('1')
    await w.find('[data-load-playlist]').trigger('click')
    await flushPromises()
    expect(s.putsDe('load_playlist')).toEqual([
      { op: 'load_playlist', name: 'Jazz', where: 'nas' },
    ])
  })

  it('sans liste enregistrée, le volet le dit', async () => {
    const { w } = await monter()
    expect(w.find('[data-no-saved]').text()).toBe('Aucune liste enregistrée')
    expect(w.find('[data-empty-playlist]').text()).toBe('Liste vide')
  })
})
