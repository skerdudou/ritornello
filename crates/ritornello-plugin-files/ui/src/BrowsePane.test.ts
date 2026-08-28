import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOG, server } from './harness'

/** Un niveau tel que `scan::list_dir` le rend : des **noms**, pas des chemins. */
interface Niveau {
  dirs: string[]
  files: string[]
  /** Fichiers `.m3u`/`.m3u8` : ils se chargent, ils ne s'ajoutent pas. */
  playlists?: string[]
}

/**
 * Simulacre d'un partage : le plugin ne rend qu'**un** niveau par `browse`,
 * celui qu'on lui demande, et il range journey et recherche au même endroit.
 * `query` est ce qui les distingue.
 */
function arbre(
  niveaux: Record<string, Niveau>,
  trouvailles: string[] = [],
  truncated = false,
  abort = false,
) {
  const s = server({ roots: [{ name: 'nas', kind: 'smb', host: 'h', share: 'musique' }] })
  s.surPut = (charge) => {
    const path = String(charge.path ?? '')
    if (charge.op === 'browse') {
      const n = niveaux[path] ?? { dirs: [], files: [] }
      s.data.browse = {
        root: 'nas',
        path: path,
        query: '',
        dirs: n.dirs,
        files: n.files,
        playlists: n.playlists ?? [],
        results: [],
      }
    }
    if (charge.op === 'search') {
      s.data.browse = {
        root: 'nas',
        path: path,
        query: String(charge.query ?? ''),
        dirs: [],
        files: [],
        results: trouvailles,
        truncated: truncated,
        gave_up: abort,
      }
    }
  }
  return s
}

const NIVEAUX: Record<string, Niveau> = {
  '': { dirs: ['Albums'], files: ['jingle.mp3'] },
  Albums: { dirs: ['Jazz'], files: ['01.mp3'], playlists: ['tout.m3u'] },
  'Albums/Jazz': { dirs: [], files: ['Kind of Blue.flac'] },
}

async function monterArbre(trouvailles: string[] = [], truncated = false, abort = false) {
  const s = arbre(NIVEAUX, trouvailles, truncated, abort)
  const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
  await flushPromises()
  return { w, s }
}

/** Noms affichés dans le niveau courant, dossiers comme fichiers. */
function noms(w: ReturnType<typeof mount>): string[] {
  return w.findAll('[data-browse-name]').map((n) => n.text())
}

describe('navigateur de fichiers', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('n’ouvre qu’un niveau au chargement de la page', async () => {
    // Régression encodée : demander l'arborescence entière d'un partage de
    // plusieurs dizaines de milliers de fichiers dépasserait de loin le plafond
    // de 5 s du cœur — la page n'afficherait rien du tout.
    const { w, s } = await monterArbre()
    expect(s.putsDe('browse')).toEqual([{ op: 'browse', root: 'nas', path: '' }])
    expect(noms(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('descend dans un dossier et REMPLACE le niveau affiché', async () => {
    // Un navigateur, pas un arbre : c'est ce qui borne la hauteur de la liste.
    // Le path envoyé est recomposé — `Albums/Jazz` et non `Jazz`, que le
    // plugin résoudrait contre la root, donc ailleurs.
    const { w, s } = await monterArbre()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums'])
    expect(noms(w)).toEqual(['Jazz', 'tout.m3u', '01.mp3'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums', 'Albums/Jazz'])
    expect(noms(w)).toEqual(['Kind of Blue.flac'])
  })

  it('remonte au parent, et ne le propose pas au sommet', async () => {
    const { w, s } = await monterArbre()
    expect(w.find('[data-browse-up]').attributes('disabled')).toBeDefined()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-browse-up]').attributes('disabled')).toBeUndefined()
    await w.find('[data-browse-up]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums', ''])
    expect(noms(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('affiche le path ouvert, root comprise', async () => {
    // Sans le nom de la root, un path relatif ne dit pas où l'on se trouve
    // quand plusieurs sources sont déclarées.
    const { w } = await monterArbre()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-browse-path]').attributes('title')).toBe('nas/Albums')
  })

  it('ajoute le dossier ouvert, sauf au sommet', async () => {
    // Au sommet le geste existe déjà sur la ligne de la source (volet Sources) :
    // deux boutons pour le même effet faisaient search une différence qui
    // n'existait pas.
    const { w, s } = await monterArbre()
    expect(w.find('[data-add-current]').exists()).toBe(false)
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-add-current]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
  })

  it('ajoute un dossier listé de façon récursive, et un fichier seul', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-add-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
    await w.find('[data-add-file]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_file')).toEqual([{ op: 'add_file', root: 'nas', path: 'jingle.mp3' }])
  })

  it('un m3u se **charge**, il ne s’ajoute pas', async () => {
    // L'action est délibérément différente de celle des tracks : une liste
    // remplace la liste en cours. Les confondre ferait ajouter un fichier texte
    // que mpv tenterait de jouer.
    const { w, s } = await monterArbre()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    const rangees = w.findAll('[data-browse-row]')
    const rangeeM3u = rangees.find((r) => r.find('[data-browse-name]').text() === 'tout.m3u')
    expect(rangeeM3u).toBeDefined()
    expect(rangeeM3u!.find('[data-add-file]').exists()).toBe(false)
    await rangeeM3u!.find('[data-load-m3u]').trigger('click')
    await flushPromises()
    expect(s.putsDe('load_m3u')).toEqual([
      { op: 'load_m3u', root: 'nas', path: 'Albums/tout.m3u' },
    ])
    expect(s.putsDe('add_file')).toEqual([])
  })

  it('cherche dans le dossier ouvert, sans effacer le niveau affiché', async () => {
    // Les deux vivent au même endroit côté plugin : si la page lisait le niveau
    // dans la réponse, une recherche viderait la liste sous les yeux.
    const { w, s } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsDe('search')).toEqual([
      { op: 'search', root: 'nas', path: 'Albums', query: 'miles' },
    ])
    // Le path complet, pas seulement le nom : une recherche rapporte des
    // homonymes venus de dossiers différents, et rien d'autre ne les distingue.
    expect(w.find('[data-search-row]').text()).toContain('Albums/Jazz/miles.flac')
    expect(noms(w)).toContain('Jazz')
  })

  it('dit sur quel dossier la recherche porte', async () => {
    const { w } = await monterArbre()
    expect(w.find('[data-search-scope]').text()).toContain('nas')
  })

  it('ajoute un résultat de recherche par son path', async () => {
    const { w, s } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    await w.find('[data-add-result]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_file')).toEqual([
      { op: 'add_file', root: 'nas', path: 'Albums/Jazz/miles.flac' },
    ])
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

  it('une recherche abandonnée ne se fait pas passer pour « aucun résultat »', async () => {
    // Défaut de revue : la marche rend `Ok(true)` que le plafond atteint soit
    // celui des visites ou celui des résultats, et « Aucun résultat » —
    // « ce fichier n'existe pas » — s'affichait pour une recherche qui avait
    // simplement renoncé avant d'arriver jusqu'au fichier cherché.
    const { w } = await monterArbre([], false, true)
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-no-results]').exists()).toBe(false)
    expect(w.find('[data-search-gave-up]').exists()).toBe(true)
  })

  it('descend après une recherche efface les résultats, pas seulement le niveau', async () => {
    // Symptôme observé : `search_scope` est une légende vivante (`computed`
    // sur le dossier ouvert). Sans effacer `results`/`query` au changement de
    // dossier, on obtient des résultats de « miles » dans Albums affichés sous
    // une légende qui annonce déjà « Jazz », et un champ de recherche qui
    // semble toujours actif alors qu'il ne correspond plus à rien.
    const { w } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-results]').exists()).toBe(true)
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-results]').exists()).toBe(false)
    expect((w.find('[data-search-query]').element as HTMLInputElement).value).toBe('')
  })

  it('une recherche vide n’émet rien', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-search-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsDe('search')).toHaveLength(0)
  })

  it('ne prend pas une réponse de recherche pour un niveau', async () => {
    // Garde-fou du marqueur `query` : sans lui, la réponse d'une recherche
    // portant sur le dossier ouvert remplirait le niveau avec ses résultats,
    // c'est-à-dire avec rien du tout (`dirs` et `files` y sont vides).
    const { w } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(noms(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('un refus de journey ne fait pas passer le dossier pour vide', async () => {
    // Mémoriser un niveau vide après un refus le ferait passer pour un dossier
    // vide, et l'utilisateur n'aurait aucun moyen de réessayer sans reload
    // la page.
    const s = arbre(NIVEAUX)
    s.refus = 'could not read "Albums": the share may be unreachable'
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(w.find('[data-message]').text()).toBe(s.refus)
    expect(w.find('[data-browse-empty]').exists()).toBe(false)
  })

  it('sans root déclarée, le volet le dit au lieu d’émettre un journey', async () => {
    const s = server()
    const w = mount(FilesAdmin, { props: { catalog: CATALOG, base: BASE } })
    await flushPromises()
    expect(s.putsDe('browse')).toHaveLength(0)
    // Même phrase que le volet Sources : deux formulations pour le même vide
    // laisseraient croire à deux causes différentes.
    expect(w.find('[data-volet-parcourir]').text()).toContain('Aucune source déclarée')
  })
})
