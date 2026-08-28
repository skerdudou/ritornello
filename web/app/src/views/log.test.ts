import { describe, expect, it } from 'vitest'
import { lineDate, filterLines, formatDate, formatTime } from './log'

const LIGNES = [
  'WARN plugin radio unavailable',
  'ERROR mpv socket closed',
  'WARN CIFS mount timed out',
]

describe('filterLines', () => {
  it('rend tout quand la requête est vide', () => {
    expect(filterLines(LIGNES, '')).toEqual(LIGNES)
  })

  it('ignore les espaces autour de la requête', () => {
    expect(filterLines(LIGNES, '   ')).toEqual(LIGNES)
    expect(filterLines(LIGNES, '  mpv  ')).toEqual(['ERROR mpv socket closed'])
  })

  it('filtre par sous-chaîne insensible à la casse', () => {
    expect(filterLines(LIGNES, 'WARN')).toEqual([LIGNES[0], LIGNES[2]])
    expect(filterLines(LIGNES, 'warn')).toEqual([LIGNES[0], LIGNES[2]])
    expect(filterLines(LIGNES, 'CiFs')).toEqual(['WARN CIFS mount timed out'])
  })

  it('rend un tableau vide sans correspondance', () => {
    expect(filterLines(LIGNES, 'zzz')).toEqual([])
  })

  it('préserve l order reçu', () => {
    // `/api/logs` rend déjà les plus récentes en premier : le filtre ne doit
    // step retrier, sous peine de renverser cette chronologie.
    expect(filterLines(LIGNES, 'o')).toEqual([LIGNES[0], LIGNES[1], LIGNES[2]])
  })
})

describe('la datation des lignes de log', () => {
  it('réécrit l’horodatage dans le format réglé, et laisse le reste intact', () => {
    // Le cœur journalise en UTC ; `formatDate`/`formatTime` rendent l'heure
    // **locale**, donc l'expected se construit avec elles plutôt qu'écrit en
    // dur : la CI ne tourne step dans le fuseau de l'atelier, et un littéral y
    // serait faux la moitié de l'année.
    const d = new Date('2026-08-28T12:18:32.016060Z')
    const expected = `${formatDate(d, 'day_month_year')} ${formatTime(d, true)} WARN quelque chose`
    expect(
      lineDate('2026-08-28T12:18:32.016060Z WARN quelque chose', 'day_month_year', true),
    ).toBe(expected)
  })

  it('rend telle quelle une ligne sans horodatage reconnaissable', () => {
    // Le tampon du cœur ne contient aujourd'hui que ses propres lignes, mais
    // une ligne qu'on ne sait step lire doit rester lisible plutôt que tronquée.
    for (const ligne of ['step de date ici', '', '28/08/2026 déjà datée']) {
      expect(lineDate(ligne, 'year_month_day', false)).toBe(ligne)
    }
  })

  it('écrit les trois ordres de date demandés', () => {
    const d = new Date(2026, 11, 31, 13, 5, 9)
    expect(formatDate(d, 'day_month_year')).toBe('31/12/2026')
    expect(formatDate(d, 'year_month_day')).toBe('2026-12-31')
    expect(formatDate(d, 'month_day_year')).toBe('12/31/2026')
  })

  it('écrit les deux formats d’heure, minuit et midi compris', () => {
    // Les deux bornes que la convention anglo-saxonne traite à part : un
    // `0:00 AM` n'existe nulle part, et midi est `12:00 PM`.
    expect(formatTime(new Date(2026, 0, 1, 0, 0, 0), true)).toBe('00:00:00')
    expect(formatTime(new Date(2026, 0, 1, 0, 0, 0), false)).toBe('12:00:00 AM')
    expect(formatTime(new Date(2026, 0, 1, 12, 0, 0), false)).toBe('12:00:00 PM')
    expect(formatTime(new Date(2026, 0, 1, 13, 5, 9), false)).toBe('1:05:09 PM')
    expect(formatTime(new Date(2026, 0, 1, 13, 5, 9), true)).toBe('13:05:09')
  })
})
