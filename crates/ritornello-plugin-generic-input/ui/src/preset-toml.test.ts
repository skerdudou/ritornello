import { describe, expect, it } from 'vitest'
import { ACTIONS, codesFor, collect, presetToml, sanitiseDeviceName } from './preset-toml'

describe('ACTIONS', () => {
  it('couvre les 23 actions du protocole', () => {
    expect(ACTIONS).toHaveLength(23)
    expect(ACTIONS.slice(0, 9).map((a) => a.cmd.arg)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9])
    expect(ACTIONS.slice(0, 9).every((a) => a.cmd.cmd === 'Select')).toBe(true)
    expect(ACTIONS.slice(9).map((a) => a.cmd.cmd)).toEqual([
      'Select', 'Plus10', 'VolumeUp', 'VolumeDown', 'Mute', 'PlayPause', 'Stop',
      'SeekBackward', 'SeekForward', 'Next', 'Prev', 'Eject', 'SourceCycle', 'Power',
    ])
    // La touche 0 et +10 s'inserent juste apres act_select_9.
    expect(ACTIONS[9]).toEqual({ key: 'act_select_0', cmd: { cmd: 'Select', arg: 0 } })
    expect(ACTIONS[10]).toEqual({ key: 'act_plus10', cmd: { cmd: 'Plus10' } })
  })

  it('offre les deux actions de deplacement, apres le transport', () => {
    const cles = ACTIONS.map((a) => a.key)
    expect(cles).toContain('act_seek_back')
    expect(cles).toContain('act_seek_forward')
    expect(cles.indexOf('act_seek_back')).toBeLessThan(cles.indexOf('act_seek_forward'))
  })
})

describe('codesFor', () => {
  const table = {
    devices: [
      { name: 'mce', bindings: [{ code: 1, cmd: 'Select', arg: 1 }, { code: 2, cmd: 'Select', arg: 1 }, { code: 9, cmd: 'Mute' }] },
      { name: 'clavier', bindings: [{ code: 5, cmd: 'Mute' }] },
    ],
  }

  it('joint les codes d’une même action, séparés par des virgules', () => {
    expect(codesFor(table, 'mce', { cmd: 'Select', arg: 1 })).toBe('1, 2')
  })

  it('distingue une commande sans argument', () => {
    expect(codesFor(table, 'mce', { cmd: 'Mute' })).toBe('9')
    expect(codesFor(table, 'clavier', { cmd: 'Mute' })).toBe('5')
  })

  it('renvoie une chaîne vide pour un périphérique ou une action absents', () => {
    expect(codesFor(table, 'inconnu', { cmd: 'Mute' })).toBe('')
    expect(codesFor(table, 'clavier', { cmd: 'Power' })).toBe('')
  })
})

describe('collect', () => {
  it('réécrit le périphérique courant et préserve les autres tels quels', () => {
    const table = {
      devices: [
        { name: 'mce', bindings: [{ code: 1, cmd: 'Select', arg: 1 }] },
        { name: 'clavier', bindings: [{ code: 5, cmd: 'Mute' }] },
      ],
    }
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? '7' : ''))
    const out = collect(table, 'mce', codes)
    expect(out.devices.find((d) => d.name === 'clavier')).toEqual(table.devices[1])
    expect(out.devices.find((d) => d.name === 'mce')!.bindings).toEqual([{ code: 7, cmd: 'Mute' }])
  })

  it('accepte plusieurs codes par action et ignore ce qui n’est pas un nombre', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? ' 7 , 8 , abc , ' : ''))
    const out = collect({ devices: [] }, 'mce', codes)
    expect(out.devices[0]!.bindings).toEqual([
      { code: 7, cmd: 'Mute' },
      { code: 8, cmd: 'Mute' },
    ])
  })

  it('n’émet `arg` que lorsqu’il existe', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Select' && a.cmd.arg === 3 ? '4' : ''))
    expect(collect({ devices: [] }, 'mce', codes).devices[0]!.bindings).toEqual([
      { code: 4, cmd: 'Select', arg: 3 },
    ])
  })
})

describe('presetToml', () => {
  it('produit le format lu par `presets::parse_preset`', () => {
    // Miroir exact du format cote Rust
    // (crates/ritornello-plugin-generic-input/src/presets.rs) : toute
    // evolution du format Rust doit etre repercutee ici, sous peine de
    // fichiers exportes que le serveur refuserait.
    const out = presetToml([{ code: 4, cmd: 'Select', arg: 3 }, { code: 9, cmd: 'Mute' }])
    expect(out).toBe(
      '[[bindings]]\ncode = 4\ncmd = "Select"\narg = 3\n\n[[bindings]]\ncode = 9\ncmd = "Mute"\n',
    )
  })

  it('produit une chaîne vide sans binding', () => {
    expect(presetToml([])).toBe('')
  })
})

describe('sanitiseDeviceName', () => {
  it('réduit le nom à un identifiant de fichier sûr', () => {
    expect(sanitiseDeviceName('Media Center Ed. 3/4')).toBe('Media_Center_Ed_3_4')
    expect(sanitiseDeviceName('../../etc/passwd')).toBe('_etc_passwd')
  })
})
