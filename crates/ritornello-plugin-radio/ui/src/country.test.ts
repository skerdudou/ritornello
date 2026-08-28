import { describe, expect, it } from 'vitest'
import { countryName, displayableCountries, ALL_COUNTRIES } from './country'

const LISTE = [
  { code: 'DE', stations: 6081 },
  { code: 'FR', stations: 2746 },
  { code: 'BE', stations: 300 },
  { code: 'US', stations: 7560 },
]

describe('countryName', () => {
  it('rend le nom du country dans la langue demandee', () => {
    // C'est ce qui remplace une table de 241 country a traduire dans chaque pack.
    expect(countryName('FR', 'fr')).toBe('France')
    expect(countryName('DE', 'fr')).toBe('Allemagne')
    expect(countryName('DE', 'en')).toBe('Germany')
    // La casse et les blancs de l'annuaire ne doivent pas gener.
    expect(countryName(' be ', 'fr')).toBe('Belgique')
  })

  it('retombe sur le code plutot que de disparaitre', () => {
    // Un code inconnu du moteur doit rester selectionnable : l'annuaire en
    // renvoie ce qu'il veut, et une entree sans label serait pire qu'un code.
    //
    // `QQ` et non `ZZ` : ce dernier est un code ISO **valide** (« region
    // inconnue »), que le moteur traduit — il ne sonde donc pas le repli.
    expect(countryName('QQ', 'fr')).toBe('QQ')
    expect(countryName('', 'fr')).toBe('')
  })
})

describe('displayableCountries', () => {
  it('trie par nom lisible et non par code', () => {
    // « Allemagne » se cherche a la lettre A, pas a DE.
    const noms = displayableCountries(LISTE, '', 'fr').map((p) => p.nom)
    expect(noms).toEqual(['Allemagne', 'Belgique', 'États-Unis', 'France'])
  })

  it('filter sur le nom, sans se soucier des accents ni de la casse', () => {
    expect(displayableCountries(LISTE, 'etats', 'fr').map((p) => p.code)).toEqual(['US'])
    expect(displayableCountries(LISTE, 'ALLEM', 'fr').map((p) => p.code)).toEqual(['DE'])
    expect(displayableCountries(LISTE, 'gi', 'fr').map((p) => p.code)).toEqual(['BE'])
  })

  it('filter aussi sur le code, qu on tape quand on le connait', () => {
    expect(displayableCountries(LISTE, 'fr', 'fr').map((p) => p.code)).toEqual(['FR'])
    expect(displayableCountries(LISTE, 'us', 'fr').map((p) => p.code)).toEqual(['US'])
  })

  it('conserve le nombre de stations, qui aide a choisir', () => {
    const fr = displayableCountries(LISTE, 'france', 'fr')[0]
    expect(fr?.stations).toBe(2746)
  })

  it('rend une liste vide quand rien ne correspond', () => {
    expect(displayableCountries(LISTE, 'zzzz', 'fr')).toEqual([])
    expect(displayableCountries([], '', 'fr')).toEqual([])
  })

  it('« tous les country » est la chaine vide attendue par le plugin', () => {
    // Le contract serveur est `country: ''` ; toute sentinelle interne finirait
    // par fuir dans la requete.
    expect(ALL_COUNTRIES).toBe('')
  })
})
