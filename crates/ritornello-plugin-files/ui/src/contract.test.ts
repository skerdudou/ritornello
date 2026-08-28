import { UI_CONTRACT } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import FilesAdmin, { contract } from './index'

describe('contract du module', () => {
  it('déclare la version du contract attendue par le shell', () => {
    // Le shell compare cette valeur à la sienne avant de mount le composant :
    // un écart s'affiche comme « interface à reconstruire » plutôt que de
    // casser la page. La reprendre depuis le kit plutôt que de l'écrire en dur
    // fait qu'une incrémentation du contract n'oublie pas ce module.
    expect(contract).toBe(UI_CONTRACT)
  })

  it('exige `base`, sans valeur par défaut', () => {
    // Régression encodée : le nom sous lequel un plugin est servi vient de
    // `plugins.toml`, donc du déploiement. Un module qui se replierait sur
    // « /plugins/files/ » serait faux — silencieusement — dès qu'un opérateur
    // le déclare sous un autre nom, et toutes ses requêtes partiraient vers un
    // plugin inexistant.
    const props = (FilesAdmin as unknown as { props: Record<string, { required?: boolean; default?: unknown }> }).props
    expect(props.base!.required).toBe(true)
    expect(props.base!.default).toBeUndefined()
    expect(props.catalog!.required).toBe(true)
  })
})
