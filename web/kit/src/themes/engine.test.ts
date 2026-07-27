import { beforeEach, describe, expect, it } from 'vitest'
import {
  applyTheme,
  DEFAULT_MODE,
  DEFAULT_PRESET,
  fontFamilies,
  presets,
  resolveVars,
  withFallback,
} from './engine'

describe('presets', () => {
  it('embarque les 42 presets de l’amont, dont le défaut', () => {
    expect(Object.keys(presets)).toHaveLength(42)
    expect(presets[DEFAULT_PRESET]?.label).toBe('Northern Lights')
    expect(DEFAULT_MODE).toBe('light')
  })

  it('chaque preset a un libellé et ses deux modes', () => {
    for (const [nom, p] of Object.entries(presets)) {
      expect(p.label, nom).toBeTruthy()
      expect(p.styles.light.background, nom).toBeTruthy()
      expect(p.styles.dark.background, nom).toBeTruthy()
    }
  })
})

describe('resolveVars', () => {
  it('superpose le bloc du mode sur le bloc clair', () => {
    const preset = {
      label: 'T',
      styles: { light: { background: '#fff', radius: '0.5rem' }, dark: { background: '#000' } },
    }
    const vars = resolveVars(preset, 'dark')
    expect(vars.background).toBe('#000')
    // `radius` n'est pas redéfini par le bloc sombre : il vient du bloc clair.
    expect(vars.radius).toBe('0.5rem')
  })

  it('en mode clair, le bloc sombre est ignoré', () => {
    const preset = {
      label: 'T',
      styles: { light: { background: '#fff' }, dark: { background: '#000' } },
    }
    expect(resolveVars(preset, 'light').background).toBe('#fff')
  })
})

describe('applyTheme', () => {
  let root: HTMLElement

  beforeEach(() => {
    document.head.innerHTML = ''
    root = document.createElement('div')
  })

  it('écrit chaque clé du preset en variable CSS', () => {
    applyTheme(DEFAULT_PRESET, 'light', root)
    expect(root.style.getPropertyValue('--background')).toBe('#f9f9fa')
    expect(root.style.getPropertyValue('--primary')).toBe('#34a85a')
    expect(root.style.getPropertyValue('--radius')).toBe('0.5rem')
  })

  it('applique le bloc sombre en mode sombre et pose la classe `dark`', () => {
    applyTheme(DEFAULT_PRESET, 'dark', root)
    expect(root.style.getPropertyValue('--background')).toBe('#1a1d23')
    expect(root.classList.contains('dark')).toBe(true)
    applyTheme(DEFAULT_PRESET, 'light', root)
    expect(root.classList.contains('dark')).toBe(false)
  })

  it('applique une clé inconnue sans broncher (itération générique)', () => {
    // Aucune liste de clés en dur : un preset amont qui gagne une variable
    // doit fonctionner sans toucher au code.
    const root2 = document.createElement('div')
    applyTheme('__test__', 'light', root2, document, {
      __test__: { label: 'T', styles: { light: { 'variable-inedite': '#123456' }, dark: {} } },
    })
    expect(root2.style.getPropertyValue('--variable-inedite')).toBe('#123456')
  })

  it('purge les variables du thème précédent', () => {
    applyTheme('__a__', 'light', root, document, {
      __a__: { label: 'A', styles: { light: { 'seulement-dans-a': '#111' }, dark: {} } },
    })
    expect(root.style.getPropertyValue('--seulement-dans-a')).toBe('#111')
    applyTheme('__b__', 'light', root, document, {
      __b__: { label: 'B', styles: { light: { background: '#222' }, dark: {} } },
    })
    expect(root.style.getPropertyValue('--seulement-dans-a')).toBe('')
  })

  it('purge chaque root selon ses propres clés, sans fuite entre roots', () => {
    // Régression : `posees` était un état de module unique, partagé par tous
    // les roots. Un appel sur un root intercalé entre deux appels sur un
    // autre root écrasait la mémoire des clés de ce dernier : à son appel
    // suivant, il purgeait les clés de l'autre root (sans effet, elles n'y
    // sont pas) au lieu des siennes — une variable posée par son propre
    // appel précédent restait alors périmée sur lui. L'indexation par root
    // (`WeakMap`) élimine ce couplage.
    const rootA = document.createElement('div')
    const rootB = document.createElement('div')

    // 1) rootA reçoit une variable qu'il ne redéfinira pas ensuite.
    applyTheme('__a1__', 'light', rootA, document, {
      __a1__: { label: 'A1', styles: { light: { 'seulement-dans-a1': '#111' }, dark: {} } },
    })
    // 2) rootB reçoit un thème dans l'intervalle : avec un état global,
    // c'est cet appel qui écraserait la mémoire des clés de rootA.
    applyTheme('__b__', 'light', rootB, document, {
      __b__: { label: 'B', styles: { light: { 'seulement-dans-b': '#222' }, dark: {} } },
    })
    // 3) rootA reçoit un second thème qui ne redéfinit pas
    // `seulement-dans-a1` : celle-ci doit être purgée, en se basant sur les
    // clés que *rootA* avait lui-même posées (pas celles de rootB).
    applyTheme('__a2__', 'light', rootA, document, {
      __a2__: { label: 'A2', styles: { light: { 'seulement-dans-a2': '#333' }, dark: {} } },
    })

    expect(rootA.style.getPropertyValue('--seulement-dans-a1')).toBe('')
    expect(rootA.style.getPropertyValue('--seulement-dans-a2')).toBe('#333')
    // `rootB` n'a rien hérité des clés de rootA et conserve la sienne.
    expect(rootB.style.getPropertyValue('--seulement-dans-a1')).toBe('')
    expect(rootB.style.getPropertyValue('--seulement-dans-a2')).toBe('')
    expect(rootB.style.getPropertyValue('--seulement-dans-b')).toBe('#222')
  })

  it('ignore un identifiant de preset inconnu sans jeter', () => {
    applyTheme('preset-qui-nexiste-pas', 'light', root)
    expect(root.style.getPropertyValue('--background')).toBe('')
  })

  it('injecte un unique lien de polices et le remplace au changement', () => {
    applyTheme(DEFAULT_PRESET, 'light', root)
    const liens = () => [...document.head.querySelectorAll('link[data-ritornello-fonts]')]
    expect(liens()).toHaveLength(1)
    expect(liens()[0]?.getAttribute('href')).toContain('Plus+Jakarta+Sans')
    applyTheme('vercel', 'light', root)
    expect(liens()).toHaveLength(1)
  })
})

describe('polices', () => {
  it('extrait les familles citées, sans doublon', () => {
    const familles = fontFamilies({
      'font-sans': 'Plus Jakarta Sans, sans-serif',
      'font-mono': 'JetBrains Mono, monospace',
      'font-serif': 'Plus Jakarta Sans, serif',
      background: '#fff',
    })
    expect(familles).toEqual(['Plus Jakarta Sans', 'JetBrains Mono'])
  })

  it('ne retient pas les familles génériques seules', () => {
    expect(fontFamilies({ 'font-sans': 'system-ui, sans-serif' })).toEqual([])
  })

  it('ajoute un repli système à chaque pile de polices', () => {
    expect(withFallback('font-sans', 'Plus Jakarta Sans')).toBe(
      'Plus Jakarta Sans, system-ui, sans-serif',
    )
    expect(withFallback('font-mono', 'JetBrains Mono')).toBe('JetBrains Mono, ui-monospace, monospace')
    // Repli déjà présent : on ne le duplique pas.
    expect(withFallback('font-sans', 'Inter, sans-serif')).toBe('Inter, sans-serif')
    // Clé non typographique : valeur inchangée.
    expect(withFallback('background', '#fff')).toBe('#fff')
  })
})
