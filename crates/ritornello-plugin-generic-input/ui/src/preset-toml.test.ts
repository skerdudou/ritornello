import { describe, expect, it } from 'vitest'
import { ACTIONS, codesFor, collect, conflicts, presetToml, sanitiseDeviceName } from './preset-toml'

describe('ACTIONS', () => {
  it('covers the 23 protocol actions', () => {
    expect(ACTIONS).toHaveLength(23)
    expect(ACTIONS.slice(0, 9).map((a) => a.cmd.arg)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9])
    expect(ACTIONS.slice(0, 9).every((a) => a.cmd.cmd === 'Select')).toBe(true)
    expect(ACTIONS.slice(9).map((a) => a.cmd.cmd)).toEqual([
      'Select', 'Plus10', 'VolumeUp', 'VolumeDown', 'Mute', 'PlayPause', 'Stop',
      'SeekBackward', 'SeekForward', 'Next', 'Prev', 'Eject', 'SourceCycle', 'Power',
    ])
    // The 0 key and +10 are inserted right after act_select_9.
    expect(ACTIONS[9]).toEqual({ key: 'act_select_0', cmd: { cmd: 'Select', arg: 0 } })
    expect(ACTIONS[10]).toEqual({ key: 'act_plus10', cmd: { cmd: 'Plus10' } })
  })

  it('offers the two seek actions, after transport', () => {
    const keys = ACTIONS.map((a) => a.key)
    expect(keys).toContain('act_seek_back')
    expect(keys).toContain('act_seek_forward')
    expect(keys.indexOf('act_seek_back')).toBeLessThan(keys.indexOf('act_seek_forward'))
  })
})

describe('codesFor', () => {
  const table = {
    devices: [
      { name: 'mce', bindings: [{ code: 1, cmd: 'Select', arg: 1 }, { code: 2, cmd: 'Select', arg: 1 }, { code: 9, cmd: 'Mute' }] },
      { name: 'keyboard', bindings: [{ code: 5, cmd: 'Mute' }] },
    ],
  }

  it('joins the codes of the same action, comma-separated', () => {
    expect(codesFor(table, 'mce', { cmd: 'Select', arg: 1 })).toBe('1, 2')
  })

  it('distinguishes a command with no argument', () => {
    expect(codesFor(table, 'mce', { cmd: 'Mute' })).toBe('9')
    expect(codesFor(table, 'keyboard', { cmd: 'Mute' })).toBe('5')
  })

  it('returns an empty string for a missing device or action', () => {
    expect(codesFor(table, 'unknown', { cmd: 'Mute' })).toBe('')
    expect(codesFor(table, 'keyboard', { cmd: 'Power' })).toBe('')
  })
})

describe('collect', () => {
  it('rewrites the current device and preserves the others as-is', () => {
    const table = {
      devices: [
        { name: 'mce', bindings: [{ code: 1, cmd: 'Select', arg: 1 }] },
        { name: 'keyboard', bindings: [{ code: 5, cmd: 'Mute' }] },
      ],
    }
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? '7' : ''))
    const out = collect(table, 'mce', codes)
    expect(out.devices.find((d) => d.name === 'keyboard')).toEqual(table.devices[1])
    expect(out.devices.find((d) => d.name === 'mce')!.bindings).toEqual([{ code: 7, cmd: 'Mute' }])
  })

  it('accepts several codes per action and ignores what is not a number', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? ' 7 , 8 , abc , ' : ''))
    const out = collect({ devices: [] }, 'mce', codes)
    expect(out.devices[0]!.bindings).toEqual([
      { code: 7, cmd: 'Mute' },
      { code: 8, cmd: 'Mute' },
    ])
  })

  it('emits `arg` only when it exists', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Select' && a.cmd.arg === 3 ? '4' : ''))
    expect(collect({ devices: [] }, 'mce', codes).devices[0]!.bindings).toEqual([
      { code: 4, cmd: 'Select', arg: 3 },
    ])
  })
})

describe('presetToml', () => {
  it('produces the format read by `presets::parse_preset`', () => {
    // Exact mirror of the Rust-side format
    // (crates/ritornello-plugin-generic-input/src/presets.rs): any
    // evolution of the Rust format must be mirrored here, or exported
    // files would be rejected by the server.
    const out = presetToml([{ code: 4, cmd: 'Select', arg: 3 }, { code: 9, cmd: 'Mute' }])
    expect(out).toBe(
      '[[bindings]]\ncode = 4\ncmd = "Select"\narg = 3\n\n[[bindings]]\ncode = 9\ncmd = "Mute"\n',
    )
  })

  it('produces an empty string with no binding', () => {
    expect(presetToml([])).toBe('')
  })
})

describe('conflicts', () => {
  const empty = () => ACTIONS.map(() => '')

  it('with no code entered, no row is in conflict', () => {
    expect(conflicts(empty())).toEqual(ACTIONS.map(() => null))
  })

  it('returns one entry per action, whatever the length of the received array', () => {
    // The result's length follows `ACTIONS`, never the input's: a too-short
    // array just means empty fields…
    expect(conflicts([])).toHaveLength(ACTIONS.length)
    // … and a too-long array does not make the function throw. Codes
    // beyond `ACTIONS` are ignored: they can no longer be reported as a
    // conflict under a nonexistent action key.
    expect(conflicts([...empty(), '9', '9'])).toEqual(ACTIONS.map(() => null))
  })

  it('all-distinct codes produce no conflict', () => {
    const codes = empty()
    codes[0] = '1'
    codes[1] = '2'
    codes[2] = '3'
    expect(conflicts(codes)).toEqual(ACTIONS.map(() => null))
  })

  it('the same code on two actions puts both rows in conflict, each naming the other', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const codes = empty()
    codes[iMute] = '42'
    codes[iPower] = '42'
    const res = conflicts(codes)
    expect(res[iMute]).toEqual({ code: 42, others: ['act_power'] })
    expect(res[iPower]).toEqual({ code: 42, others: ['act_mute'] })
    expect(res.filter((c) => c !== null)).toHaveLength(2)
  })

  it('the same code on three actions lists the two other keys, in ascending index order', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const iStop = ACTIONS.findIndex((a) => a.key === 'act_stop')
    const sorted = [iMute, iPower, iStop].sort((x, y) => x - y)
    const [a, b, c] = [sorted[0]!, sorted[1]!, sorted[2]!]
    const codes = empty()
    codes[iMute] = '7'
    codes[iPower] = '7'
    codes[iStop] = '7'
    const res = conflicts(codes)
    expect(res[a]).toEqual({ code: 7, others: [ACTIONS[b]!.key, ACTIONS[c]!.key] })
    expect(res[b]).toEqual({ code: 7, others: [ACTIONS[a]!.key, ACTIONS[c]!.key] })
    expect(res[c]).toEqual({ code: 7, others: [ACTIONS[a]!.key, ACTIONS[b]!.key] })
  })

  it('a duplicate internal to the field names no other action', () => {
    const i = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const codes = empty()
    codes[i] = '115, 115'
    expect(conflicts(codes)[i]).toEqual({ code: 115, others: [] })
  })

  it('on a multi-code field, reports the second code when only it is in cross-row conflict', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const codes = empty()
    codes[iMute] = '3, 8'
    codes[iPower] = '8'
    expect(conflicts(codes)[iMute]).toEqual({ code: 8, others: ['act_power'] })
  })

  it('when the fields first code is an internal duplicate and the second is in cross-row conflict, reports the first (field order)', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const codes = empty()
    codes[iMute] = '5, 5, 8'
    codes[iPower] = '8'
    expect(conflicts(codes)[iMute]).toEqual({ code: 5, others: [] })
  })

  it('ignores spaces and non-numeric entries, reports only the duplicate', () => {
    const i = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const codes = empty()
    codes[i] = ' 9 , x , 9 '
    expect(conflicts(codes)[i]).toEqual({ code: 9, others: [] })
  })
})

describe('sanitiseDeviceName', () => {
  it('reduces the name to a safe file identifier', () => {
    expect(sanitiseDeviceName('Media Center Ed. 3/4')).toBe('Media_Center_Ed_3_4')
    expect(sanitiseDeviceName('../../etc/passwd')).toBe('_etc_passwd')
  })
})
