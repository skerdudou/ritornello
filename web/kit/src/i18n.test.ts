import { describe, expect, it } from 'vitest'
import { createT } from './i18n'

describe('createT', () => {
  it('résout une clé présente', () => {
    const t = createT({ saved: 'Enregistré' })
    expect(t('saved')).toBe('Enregistré')
  })

  it('retombe sur la clé elle-même quand elle est absente', () => {
    const t = createT({})
    expect(t('inconnue')).toBe('inconnue')
  })

  it('interpole les jetons nommés comme le fait le Rust', () => {
    const t = createT({ bad_request: 'Requête invalide : {detail}' })
    expect(t('bad_request', { detail: 'preset en double' })).toBe(
      'Requête invalide : preset en double',
    )
  })

  it('interpole un jeton numérique et laisse les jetons non fournis intacts', () => {
    const t = createT({ msg: '{n} sur {total}' })
    expect(t('msg', { n: 3 })).toBe('3 sur {total}')
  })

  it("n'interprète pas la valeur : une apostrophe droite passe telle quelle", () => {
    // C'est précisément ce que l'ancienne substitution `{{cle}}` cassait
    // (défaut Critical de dbfa771) : ici la valeur est une donnée, jamais du
    // source, donc aucun caractère n'est dangereux.
    const t = createT({ hint: "choisir d'abord un périphérique" })
    expect(t('hint')).toBe("choisir d'abord un périphérique")
  })
})
