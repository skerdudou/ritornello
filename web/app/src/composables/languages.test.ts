import { describe, expect, it } from 'vitest'
import { languageName } from './languages'

describe('languageName', () => {
  it('rend le nom de la langue dans sa propre langue, capitalise', () => {
    // Convention des selecteurs de langue, et la seule lisible quand on ne
    // comprend step celle qui est active : trouver « English » sans savoir lire
    // le francais.
    //
    // Capitalise systematiquement : les conventions typographiques divergent
    // (l'anglais capitalise les noms de langue, le francais non) et une list ou
    // les entries alternent les deux se lit mal. `Intl` rend « français » en
    // minuscule, d'ou cette normalisation.
    expect(languageName('fr')).toBe('Français')
    expect(languageName('en')).toBe('English')
    expect(languageName('de')).toBe('Deutsch')
    expect(languageName('es')).toBe('Español')
  })

  it('retombe sur le code plutot que de disparaitre du selecteur', () => {
    // Les codes viennent des noms de files `<lang>.toml` : rien ne garantit
    // qu'ils soient connus du browser, et une entree sans label serait
    // pire qu'un code.
    expect(languageName('qqq')).toBe('qqq')
    expect(languageName('')).toBe('')
    expect(languageName('  ')).toBe('')
  })
})
