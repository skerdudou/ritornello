import { flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { monter } from './harnais'

const TROIS = [
  { path: 'Albums/Jazz/01.mp3', name: 'Piste 1', duration_s: 245, missing: false },
  { path: 'Albums/Jazz/02.mp3', name: 'Piste 2', duration_s: 0, missing: true },
  { path: 'Albums/Jazz/03.mp3', name: 'Piste 3', duration_s: 3725, missing: false },
]

/**
 * Simulacre du flux poussé du cœur : jsdom n'a pas d'`EventSource`.
 *
 * À installer **avant** le montage — la page s'y abonne dans `onMounted`, et un
 * simulacre posé après ne serait jamais vu.
 */
function pousseurDeLecteur() {
  const relais: { envoyer: ((e: MessageEvent) => void) | null } = { envoyer: null }
  vi.stubGlobal(
    'EventSource',
    class {
      set onmessage(f: (e: MessageEvent) => void) {
        relais.envoyer = f
      }
      close(): void {}
    },
  )
  return {
    pousse: (etat: unknown) => {
      relais.envoyer?.({ data: JSON.stringify(etat) } as MessageEvent)
    },
  }
}

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

  it('n’accuse pas une piste dont le montage ne répondait pas', async () => {
    // `missing: null` veut dire « on ne sait pas » : le plugin n'a pas pu
    // regarder, son disjoncteur ayant coupé sur un partage muet. Afficher
    // « introuvable » accuserait le fichier d'une panne qui est celle du
    // montage — et enverrait l'utilisateur chercher un fichier qui est là.
    const { w } = await monter({
      playlist: [{ path: '/mnt/ritornello/nas/a.mp3', name: 'Sur le NAS', duration_s: 0, missing: null }],
      unresponsive: ['/mnt/ritornello/nas'],
    })
    expect(w.findAll('[data-track-missing]')).toHaveLength(0)
    const inconnu = w.findAll('[data-track-unknown]')
    expect(inconnu).toHaveLength(1)
    expect(inconnu[0]!.attributes('title')).toBe('/mnt/ritornello/nas/a.mp3')
    // La piste reste là, comme une piste introuvable : ce sont les listes qui
    // rétrécissent en silence qui coûtent des mois à diagnostiquer.
    expect(w.findAll('[data-track-row]')).toHaveLength(1)
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

  it('vider pendant la lecture demande aussi l’arrêt au cœur', async () => {
    // Défaut de conception signalé : la moitié Admin ne peut rien demander à
    // mpv — les notifications du SDK sont sans action — donc vider laissait la
    // musique continuer sur une liste désormais vide. C'est la page qui demande
    // l'arrêt, par la voie de la télécommande : un geste de l'utilisateur.
    const { w, s } = await monter({ playlist: TROIS, playing: true })
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsDe('clear')).toHaveLength(1)
    expect(s.urls()).toContain('/api/command')
  })

  it('demande l’arrêt même si la page croyait à tort que rien ne jouait', async () => {
    // Fragilité mesurée : la page ne sonde pas en continu, donc `playing` peut
    // être périmé. Le lire avant le vidage faisait taire la demande d'arrêt sans
    // que rien ne le signale. On lit donc l'état **rendu par le vidage**, qui ne
    // touche pas à `playing`.
    const { w, s } = await monter({ playlist: TROIS, playing: false })
    s.surPut = () => {
      s.data.playing = true
    }
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.urls()).toContain('/api/command')
  })

  it('demande l’arrêt quand le cœur joue cette source, même si le plugin l’ignore', async () => {
    // Défaut signalé : au démarrage, le drapeau du plugin reste à faux — mpv
    // passe brièvement inactif avant de charger le premier fichier, et le cœur
    // envoie alors un `stop()` qui l'efface. La source active, elle, vient du
    // **cœur** par le flux poussé, et ne peut donc pas dériver.
    //
    // Le nom attendu est celui de `BASE` (`mediatheque`), et non « files » :
    // c'est le déploiement qui nomme un plugin, et la page le déduit de son
    // préfixe au lieu de l'écrire en dur.
    const flux = pousseurDeLecteur()
    const { w, s } = await monter({ playlist: TROIS, playing: false })
    flux.pousse({ source: 'mediatheque' })
    await flushPromises()

    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.urls()).toContain('/api/command')
  })

  it('ne coupe rien quand le cœur joue une autre source', async () => {
    // Garde-fou : vider une liste de fichiers pendant que la radio joue ne doit
    // surtout pas la faire taire.
    const flux = pousseurDeLecteur()
    const { w, s } = await monter({ playlist: TROIS, playing: false })
    flux.pousse({ source: 'radio' })
    await flushPromises()

    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.urls()).not.toContain('/api/command')
  })

  it('vider une liste à l’arrêt ne coupe pas la source qui joue', async () => {
    // Sans cette condition, vider une liste de fichiers inactive couperait la
    // radio — le `Stop` du cœur s'applique à la source active, pas à la nôtre.
    const { w, s } = await monter({ playlist: TROIS, playing: false })
    await w.find('[data-clear]').trigger('click')
    await flushPromises()
    expect(s.putsDe('clear')).toHaveLength(1)
    expect(s.urls()).not.toContain('/api/command')
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
