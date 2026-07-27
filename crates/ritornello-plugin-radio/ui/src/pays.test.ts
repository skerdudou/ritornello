import { describe, expect, it } from 'vitest'
import { nomPays, paysAffichables, TOUS_PAYS } from './pays'

const LISTE = [
  { code: 'DE', stations: 6081 },
  { code: 'FR', stations: 2746 },
  { code: 'BE', stations: 300 },
  { code: 'US', stations: 7560 },
]

describe('nomPays', () => {
  it('rend le nom du pays dans la langue demandee', () => {
    // C'est ce qui remplace une table de 241 pays a traduire dans chaque pack.
    expect(nomPays('FR', 'fr')).toBe('France')
    expect(nomPays('DE', 'fr')).toBe('Allemagne')
    expect(nomPays('DE', 'en')).toBe('Germany')
    // La casse et les blancs de l'annuaire ne doivent pas gener.
    expect(nomPays(' be ', 'fr')).toBe('Belgique')
  })

  it('retombe sur le code plutot que de disparaitre', () => {
    // Un code inconnu du moteur doit rester selectionnable : l'annuaire en
    // renvoie ce qu'il veut, et une entree sans libelle serait pire qu'un code.
    //
    // `QQ` et non `ZZ` : ce dernier est un code ISO **valide** (« region
    // inconnue »), que le moteur traduit — il ne sonde donc pas le repli.
    expect(nomPays('QQ', 'fr')).toBe('QQ')
    expect(nomPays('', 'fr')).toBe('')
  })
})

describe('paysAffichables', () => {
  it('trie par nom lisible et non par code', () => {
    // « Allemagne » se cherche a la lettre A, pas a DE.
    const noms = paysAffichables(LISTE, '', 'fr').map((p) => p.nom)
    expect(noms).toEqual(['Allemagne', 'Belgique', 'États-Unis', 'France'])
  })

  it('filtre sur le nom, sans se soucier des accents ni de la casse', () => {
    expect(paysAffichables(LISTE, 'etats', 'fr').map((p) => p.code)).toEqual(['US'])
    expect(paysAffichables(LISTE, 'ALLEM', 'fr').map((p) => p.code)).toEqual(['DE'])
    expect(paysAffichables(LISTE, 'gi', 'fr').map((p) => p.code)).toEqual(['BE'])
  })

  it('filtre aussi sur le code, qu on tape quand on le connait', () => {
    expect(paysAffichables(LISTE, 'fr', 'fr').map((p) => p.code)).toEqual(['FR'])
    expect(paysAffichables(LISTE, 'us', 'fr').map((p) => p.code)).toEqual(['US'])
  })

  it('conserve le nombre de stations, qui aide a choisir', () => {
    const fr = paysAffichables(LISTE, 'france', 'fr')[0]
    expect(fr?.stations).toBe(2746)
  })

  it('rend une liste vide quand rien ne correspond', () => {
    expect(paysAffichables(LISTE, 'zzzz', 'fr')).toEqual([])
    expect(paysAffichables([], '', 'fr')).toEqual([])
  })

  it('« tous les pays » est la chaine vide attendue par le plugin', () => {
    // Le contrat serveur est `country: ''` ; toute sentinelle interne finirait
    // par fuir dans la requete.
    expect(TOUS_PAYS).toBe('')
  })
})
