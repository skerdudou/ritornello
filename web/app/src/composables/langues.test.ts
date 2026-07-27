import { describe, expect, it } from 'vitest'
import { nomLangue } from './langues'

describe('nomLangue', () => {
  it('rend le nom de la langue dans sa propre langue', () => {
    // Convention des selecteurs de langue, et la seule lisible quand on ne
    // comprend pas celle qui est active : trouver « English » sans savoir lire
    // le francais.
    expect(nomLangue('fr')).toBe('français')
    expect(nomLangue('en')).toBe('English')
    expect(nomLangue('de')).toBe('Deutsch')
  })

  it('retombe sur le code plutot que de disparaitre du selecteur', () => {
    // Les codes viennent des noms de fichiers `<lang>.toml` : rien ne garantit
    // qu'ils soient connus du navigateur, et une entree sans libelle serait
    // pire qu'un code.
    expect(nomLangue('qqq')).toBe('qqq')
    expect(nomLangue('')).toBe('')
    expect(nomLangue('  ')).toBe('')
  })
})
