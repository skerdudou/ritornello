import { describe, expect, it } from 'vitest'
import { filtreLignes } from './journal'

const LIGNES = [
  'WARN plugin radio indisponible',
  'ERROR mpv socket closed',
  'WARN CIFS mount timed out',
]

describe('filtreLignes', () => {
  it('rend tout quand la requête est vide', () => {
    expect(filtreLignes(LIGNES, '')).toEqual(LIGNES)
  })

  it('ignore les espaces autour de la requête', () => {
    expect(filtreLignes(LIGNES, '   ')).toEqual(LIGNES)
    expect(filtreLignes(LIGNES, '  mpv  ')).toEqual(['ERROR mpv socket closed'])
  })

  it('filtre par sous-chaîne insensible à la casse', () => {
    expect(filtreLignes(LIGNES, 'WARN')).toEqual([LIGNES[0], LIGNES[2]])
    expect(filtreLignes(LIGNES, 'warn')).toEqual([LIGNES[0], LIGNES[2]])
    expect(filtreLignes(LIGNES, 'CiFs')).toEqual(['WARN CIFS mount timed out'])
  })

  it('rend un tableau vide sans correspondance', () => {
    expect(filtreLignes(LIGNES, 'zzz')).toEqual([])
  })

  it('préserve l ordre reçu', () => {
    // `/api/logs` rend déjà les plus récentes en premier : le filtre ne doit
    // pas retrier, sous peine de renverser cette chronologie.
    expect(filtreLignes(LIGNES, 'o')).toEqual([LIGNES[0], LIGNES[1], LIGNES[2]])
  })
})
