import { toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import MusicBrainzAdmin from './MusicBrainzAdmin.vue'

// Meme approche que `MpdAdmin.test.ts` : on garde le vrai module (composants,
// `api`, ...) et on remplace uniquement les deux entrees de `toast` que
// cette vue utilise, pour pouvoir les observer sans afficher de notification.
vi.mock('@ritornello/ui', async () => {
  const reel = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...reel, toast: { ...reel.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOGUE = {
  title: 'ICY split patterns',
  intro: 'One entry per station this device has probed.',
  col_station: 'Stream',
  col_pattern: 'Pattern',
  col_origin: 'Origin',
  col_last_used: 'Last used',
  col_split_count: 'Titles split',
  col_actions: '',
  origin_standard: 'standard, confirmed',
  origin_learned: 'learned deviation',
  origin_manual: 'manual',
  pattern_no_split: 'do not split',
  pattern_artist_first: 'artist first',
  pattern_title_first: 'title first',
  filter_exceptions_only: 'Exceptions only',
  empty: 'No station probed yet.',
  empty_filtered: 'No exception: every probed station follows the standard format.',
  edit: 'Edit',
  delete: 'Delete',
  clear_all: 'Clear all',
  save: 'Save',
  cancel: 'Cancel',
  field_separator: 'Separator',
  field_order: 'Order',
  field_no_split: 'Do not split this station',
  separator_empty: 'the separator cannot be empty',
  separator_no_space: 'the separator must contain a space on each side, otherwise a hyphenated name gets cut in two',
  unknown_station: 'no entry for that stream',
  save_failed: 'could not write the pattern file',
}

// Prefixe absolu que le shell passe par la prop `base` (requise) : c'est le
// contrat, cette vue ne connait pas le nom sous lequel elle est servie.
const BASE = '/plugins/musicbrainz/'

const STATION_CONFORME = {
  url: 'http://icecast.radiofrance.fr/franceinter-midfi.mp3',
  motif: { separe: { separateur: ' - ', artiste_en_premier: true } },
  origine: 'standard_confirme',
  dernier_usage: '2026-08-26T15:32:09Z',
  titres_decoupes: 214,
}

const STATION_EXCEPTION = {
  url: 'http://exemple/parlotte.mp3',
  motif: 'ne_pas_decouper',
  origine: 'deviation_apprise',
  dernier_usage: null,
  titres_decoupes: 0,
}

/** Monte le composant avec un `fetch` espionne : `donnees` sert le GET, les PUT
 *  sont journalises dans `puts` et repondent 204 sauf si `reponsePut` est
 *  fourni (pour simuler un refus). */
async function monter(
  donnees: { stations: unknown[] } = { stations: [STATION_CONFORME, STATION_EXCEPTION] },
  reponsePut?: (corps: { action: string }) => Response,
) {
  const puts: Array<{ url: string; corps: Record<string, unknown> }> = []
  const gets: string[] = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      const corps = JSON.parse(String(init.body)) as { action: string }
      puts.push({ url, corps })
      if (reponsePut) return reponsePut(corps)
      return new Response(null, { status: 204 })
    }
    gets.push(url)
    return new Response(JSON.stringify(donnees), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(MusicBrainzAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, spy, puts, gets }
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
})

describe('MusicBrainzAdmin', () => {
  it('masque les stations conformes par defaut', async () => {
    const { w } = await monter()

    // Le filtre est actif des le premier rendu (pas besoin d'un clic) :
    // l'absence de cette case cochee laisserait la station conforme visible
    // au premier chargement.
    expect((w.get('[data-filtre-exceptions]').element as HTMLInputElement).checked).toBe(true)

    const lignes = w.findAll('[data-station-ligne]')
    expect(lignes).toHaveLength(1)
    expect(w.text()).toContain('parlotte.mp3')
    expect(w.text()).not.toContain('franceinter-midfi.mp3')
  })

  it('les montre quand on decoche le filtre', async () => {
    const { w } = await monter()

    await w.get('[data-filtre-exceptions]').setValue(false)

    const lignes = w.findAll('[data-station-ligne]')
    expect(lignes).toHaveLength(2)
    expect(w.text()).toContain('franceinter-midfi.mp3')
    expect(w.text()).toContain('parlotte.mp3')
  })

  it('distingue « rien de sonde » de « aucune exception »', async () => {
    // Rien n'a jamais ete sonde : la liste brute est vide.
    const { w: vide } = await monter({ stations: [] })
    expect(vide.find('[data-empty]').exists()).toBe(true)
    expect(vide.find('[data-empty-filtered]').exists()).toBe(false)
    expect(vide.text()).toContain('No station probed yet.')

    // Des stations ont ete sondees, mais toutes sont conformes : le filtre
    // (actif par defaut) les masque toutes, ce qui est une information
    // opposee a « rien de sonde ».
    const { w: filtre } = await monter({ stations: [STATION_CONFORME] })
    expect(filtre.find('[data-empty-filtered]').exists()).toBe(true)
    expect(filtre.find('[data-empty]').exists()).toBe(false)
    expect(filtre.text()).toContain('No exception: every probed station follows the standard format.')
  })

  it('« ne pas decouper » grise le separateur et l ordre', async () => {
    const { w } = await monter()

    await w.get('[data-editer]').trigger('click')

    const champSeparateur = w.get('[data-separateur]').element as HTMLInputElement
    const champOrdre = w.get('[data-ordre]').element as HTMLSelectElement
    // La station exception est deja `ne_pas_decouper` : les champs sont donc
    // deja grises a l'ouverture.
    expect(champSeparateur.disabled).toBe(true)
    expect(champOrdre.disabled).toBe(true)

    // Decocher rend les champs de nouveau modifiables.
    await w.get('[data-ne-pas-decouper]').setValue(false)
    expect((w.get('[data-separateur]').element as HTMLInputElement).disabled).toBe(false)
    expect((w.get('[data-ordre]').element as HTMLSelectElement).disabled).toBe(false)

    // Et le recocher grise de nouveau les deux.
    await w.get('[data-ne-pas-decouper]').setValue(true)
    expect((w.get('[data-separateur]').element as HTMLInputElement).disabled).toBe(true)
    expect((w.get('[data-ordre]').element as HTMLSelectElement).disabled).toBe(true)
  })

  it('poste une action pose avec un motif du jeu ferme', async () => {
    const { w, puts } = await monter()

    await w.get('[data-editer]').trigger('click')
    // La station exception ouvre sur « ne pas decouper » coche : il faut le
    // decocher pour atteindre le motif separe.
    await w.get('[data-ne-pas-decouper]').setValue(false)
    await w.get('[data-separateur]').setValue(' :: ')
    await w.get('[data-ordre]').setValue('title_first')
    await w.get('[data-enregistrer-edition]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.url).toBe('/plugins/musicbrainz/api/data')
    // Un motif du jeu ferme : jamais de champ d'expression rationnelle, juste
    // un separateur et un booleen d'ordre.
    expect(puts[0]!.corps).toEqual({
      action: 'pose',
      url: 'http://exemple/parlotte.mp3',
      motif: { separe: { separateur: ' :: ', artiste_en_premier: false } },
    })
  })

  it('poste une action supprime, puis rafraichit', async () => {
    const { w, puts, gets } = await monter()
    const getsAvant = gets.length

    await w.get('[data-supprimer]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.corps).toEqual({ action: 'supprime', url: 'http://exemple/parlotte.mp3' })
    // La suppression rafraichit la liste : un second GET part apres le PUT.
    expect(gets.length).toBeGreaterThan(getsAvant)
  })

  it('affiche l erreur du dorsal telle quelle', async () => {
    const messageServeur = 'no entry for that stream'
    const { w } = await monter(
      { stations: [STATION_EXCEPTION] },
      () => new Response(JSON.stringify({ error: messageServeur }), { status: 422 }),
    )

    await w.get('[data-supprimer]').trigger('click')
    await flushPromises()

    // Le message affiche est exactement le texte renvoye par le serveur —
    // deja une phrase traduite cote Rust — jamais retraduit ni remplace par
    // une exception JS.
    expect(toast.error).toHaveBeenCalledWith(messageServeur)
  })

  it('le bouton Vider poste une action vide et rafraichit', async () => {
    const { w, puts, gets } = await monter()
    const getsAvant = gets.length

    await w.get('[data-vider]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.corps).toEqual({ action: 'vide' })
    expect(gets.length).toBeGreaterThan(getsAvant)
  })
})
