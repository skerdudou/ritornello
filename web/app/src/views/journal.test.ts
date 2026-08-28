import { describe, expect, it } from 'vitest'
import { dateeLigne, filtreLignes, formateDate, formateHeure } from './journal'

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

describe('la datation des lignes de journal', () => {
  it('réécrit l’horodatage dans le format réglé, et laisse le reste intact', () => {
    // Le cœur journalise en UTC ; `formateDate`/`formateHeure` rendent l'heure
    // **locale**, donc l'attendu se construit avec elles plutôt qu'écrit en
    // dur : la CI ne tourne pas dans le fuseau de l'atelier, et un littéral y
    // serait faux la moitié de l'année.
    const d = new Date('2026-08-28T12:18:32.016060Z')
    const attendu = `${formateDate(d, 'day_month_year')} ${formateHeure(d, true)} WARN quelque chose`
    expect(
      dateeLigne('2026-08-28T12:18:32.016060Z WARN quelque chose', 'day_month_year', true),
    ).toBe(attendu)
  })

  it('rend telle quelle une ligne sans horodatage reconnaissable', () => {
    // Le tampon du cœur ne contient aujourd'hui que ses propres lignes, mais
    // une ligne qu'on ne sait pas lire doit rester lisible plutôt que tronquée.
    for (const ligne of ['pas de date ici', '', '28/08/2026 déjà datée']) {
      expect(dateeLigne(ligne, 'year_month_day', false)).toBe(ligne)
    }
  })

  it('écrit les trois ordres de date demandés', () => {
    const d = new Date(2026, 11, 31, 13, 5, 9)
    expect(formateDate(d, 'day_month_year')).toBe('31/12/2026')
    expect(formateDate(d, 'year_month_day')).toBe('2026-12-31')
    expect(formateDate(d, 'month_day_year')).toBe('12/31/2026')
  })

  it('écrit les deux formats d’heure, minuit et midi compris', () => {
    // Les deux bornes que la convention anglo-saxonne traite à part : un
    // `0:00 AM` n'existe nulle part, et midi est `12:00 PM`.
    expect(formateHeure(new Date(2026, 0, 1, 0, 0, 0), true)).toBe('00:00:00')
    expect(formateHeure(new Date(2026, 0, 1, 0, 0, 0), false)).toBe('12:00:00 AM')
    expect(formateHeure(new Date(2026, 0, 1, 12, 0, 0), false)).toBe('12:00:00 PM')
    expect(formateHeure(new Date(2026, 0, 1, 13, 5, 9), false)).toBe('1:05:09 PM')
    expect(formateHeure(new Date(2026, 0, 1, 13, 5, 9), true)).toBe('13:05:09')
  })
})
