import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOGUE, serveur } from './harnais'

/** Un niveau tel que `scan::list_dir` le rend : des **noms**, pas des chemins. */
interface Niveau {
  dirs: string[]
  files: string[]
}

/**
 * Simulacre d'un partage : le plugin ne rend qu'**un** niveau par `browse`,
 * celui qu'on lui demande. C'est ce qui rend la paresse observable — un
 * simulacre qui rendrait tout l'arbre d'un coup laisserait passer une page qui
 * demande tout au chargement.
 */
function arbre(niveaux: Record<string, Niveau>, trouvailles: string[] = [], tronque = false) {
  const s = serveur({ roots: [{ name: 'nas', kind: 'smb', host: 'h', share: 'musique' }] })
  s.surPut = (charge) => {
    const chemin = String(charge.path ?? '')
    if (charge.op === 'browse') {
      const n = niveaux[chemin] ?? { dirs: [], files: [] }
      s.data.browse = { root: 'nas', path: chemin, dirs: n.dirs, files: n.files, results: [] }
    }
    // Parcours et recherche se rangent au **même endroit** côté plugin : une
    // recherche efface le niveau parcouru, et réciproquement.
    if (charge.op === 'search') {
      s.data.browse = {
        root: 'nas',
        path: '',
        dirs: [],
        files: [],
        results: trouvailles,
        truncated: tronque,
      }
    }
  }
  return s
}

const NIVEAUX: Record<string, Niveau> = {
  '': { dirs: ['Albums'], files: ['jingle.mp3'] },
  Albums: { dirs: ['Jazz'], files: ['01.mp3'] },
  'Albums/Jazz': { dirs: [], files: ['Kind of Blue.flac'] },
}

async function monterArbre(trouvailles: string[] = [], tronque = false) {
  const s = arbre(NIVEAUX, trouvailles, tronque)
  const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, s }
}

describe('volet de parcours', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('ne demande qu’un niveau à l’ouverture de la page', async () => {
    // Régression encodée : demander l'arborescence entière d'un partage de
    // plusieurs dizaines de milliers de fichiers dépasserait de loin le plafond
    // de 5 s du cœur — la page n'afficherait rien du tout, et le seul indice
    // serait « plugin injoignable ».
    const { w, s } = await monterArbre()
    expect(s.putsDe('browse')).toEqual([{ op: 'browse', root: 'nas', path: '' }])
    expect(w.findAll('[data-tree-row]')).toHaveLength(2)
  })

  it('ne demande un niveau qu’à son ouverture, et une seule fois', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-tree-toggle]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums'])
    // Albums, Jazz, 01.mp3, jingle.mp3
    expect(w.findAll('[data-tree-row]')).toHaveLength(4)

    // Replier puis rouvrir ne coûte aucune requête : le niveau est mémorisé.
    await w.find('[data-tree-toggle]').trigger('click')
    await flushPromises()
    expect(w.findAll('[data-tree-row]')).toHaveLength(2)
    await w.find('[data-tree-toggle]').trigger('click')
    await flushPromises()
    expect(w.findAll('[data-tree-row]')).toHaveLength(4)
    expect(s.putsDe('browse')).toHaveLength(2)
  })

  it('descend d’un niveau supplémentaire sans redemander les précédents', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-tree-toggle]').trigger('click')
    await flushPromises()
    // Le deuxième bouton de pliage est celui de « Jazz », affiché en retrait.
    await w.findAll('[data-tree-toggle]')[1]!.trigger('click')
    await flushPromises()
    // Le chemin envoyé est bien recomposé — `Albums/Jazz` et non `Jazz`, que le
    // plugin résoudrait contre la racine, donc ailleurs.
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums', 'Albums/Jazz'])
    expect(w.text()).toContain('Kind of Blue.flac')
  })

  it('ajoute un dossier de façon récursive, et un fichier seul', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-add-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
    await w.find('[data-add-file]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_file')).toEqual([{ op: 'add_file', root: 'nas', path: 'jingle.mp3' }])
  })

  it('propose d’ajouter la racine entière, au chemin vide', async () => {
    // Le cas courant sur un partage dédié à la musique : rien à parcourir, on
    // prend tout.
    const { w, s } = await monterArbre()
    await w.find('[data-add-root-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: '' }])
  })

  it('cherche dans la racine choisie et affiche les chemins trouvés', async () => {
    const { w, s } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsDe('search')).toEqual([{ op: 'search', root: 'nas', query: 'miles' }])
    // Le chemin complet, pas seulement le nom : une recherche rapporte des
    // homonymes venus de dossiers différents, et rien d'autre ne les distingue.
    expect(w.find('[data-search-row]').text()).toContain('Albums/Jazz/miles.flac')
  })

  it('signale une recherche tronquée au lieu de la présenter comme complète', async () => {
    // Régression encodée : `scan::search` plafonne à 200 résultats et le dit
    // par `truncated`. Sans cette phrase, l'utilisateur qui ne voit pas son
    // fichier conclut qu'il n'est pas là.
    const { w } = await monterArbre(['Albums/Jazz/miles.flac'], true)
    await w.find('[data-search-query]').setValue('a')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-truncated]').text()).toContain('affinez')
  })

  it('une recherche vide n’émet rien', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-search-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsDe('search')).toHaveLength(0)
  })

  it('un refus de parcours ne fait pas passer le dossier pour vide', async () => {
    // Mémoriser un niveau vide après un refus le ferait passer pour un dossier
    // vide, et l'utilisateur n'aurait aucun moyen de réessayer sans recharger
    // la page.
    const s = arbre(NIVEAUX)
    s.refus = 'could not read "Albums": the share may be unreachable'
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(w.find('[data-message]').text()).toBe(s.refus)
    expect(w.find('[data-tree-empty]').exists()).toBe(false)
  })

  it('sans racine déclarée, le volet le dit au lieu d’émettre un parcours', async () => {
    const s = serveur()
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(s.putsDe('browse')).toHaveLength(0)
    expect(w.find('[data-volet-parcourir]').text()).toContain('Aucune racine')
  })
})
