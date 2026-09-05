import { describe, expect, it } from 'vitest'
import {
  ACTIONS, type BindingTable, codesFor, collect, conflicts, presetToml, type Row, rowLabel,
  sanitiseDeviceName, sourceRows,
} from './preset-toml'

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
    const out = collect(table, 'mce', ACTIONS, codes)
    expect(out.devices.find((d) => d.name === 'keyboard')).toEqual(table.devices[1])
    expect(out.devices.find((d) => d.name === 'mce')!.bindings).toEqual([{ code: 7, cmd: 'Mute' }])
  })

  it('accepts several codes per action and ignores what is not a number', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? ' 7 , 8 , abc , ' : ''))
    const out = collect({ devices: [] }, 'mce', ACTIONS, codes)
    expect(out.devices[0]!.bindings).toEqual([
      { code: 7, cmd: 'Mute' },
      { code: 8, cmd: 'Mute' },
    ])
  })

  it('emits `arg` only when it exists', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Select' && a.cmd.arg === 3 ? '4' : ''))
    expect(collect({ devices: [] }, 'mce', ACTIONS, codes).devices[0]!.bindings).toEqual([
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

  it('quotes a string argument and leaves a number bare', () => {
    // A source shortcut carries a name, not a digit. Without the quotes the
    // export reads `arg = radio`, which is not TOML at all, and the server
    // refuses the file on re-import.
    const toml = presetToml([
      { code: 100, cmd: 'SelectSource', arg: 'radio' },
      { code: 2, cmd: 'Select', arg: 1 },
    ])
    expect(toml).toContain('arg = "radio"')
    expect(toml).toContain('arg = 1')
    expect(toml).not.toContain('arg = radio\n')
  })

  it('writes a boolean argument unquoted', () => {
    // `SetRandom` carries true/false; TOML booleans are bare words.
    expect(presetToml([{ code: 50, cmd: 'SetRandom', arg: true }])).toContain('arg = true')
  })
})

describe('rowLabel', () => {
  it('translates a keyed row and passes a labelled row through', () => {
    const t = (k: string) => (k === 'act_stop' ? 'Arrêt' : `!${k}!`)
    expect(rowLabel({ key: 'act_stop', cmd: { cmd: 'Stop' } }, t)).toBe('Arrêt')
    // A source row has no catalogue key: its label is the source name, and
    // translating it would print the raw name at best.
    expect(rowLabel({ label: 'radio', cmd: { cmd: 'SelectSource', arg: 'radio' } }, t)).toBe('radio')
  })
})

describe('conflicts', () => {
  // Identity translator: `others` then equals the row's own `key`, so the
  // pre-existing expectations below (written when `conflicts` dealt only in
  // keys) stay valid unchanged -- they still prove the row-selection logic,
  // now exercised through `rowLabel` rather than a bare `.key` lookup.
  const t = (k: string) => k
  const empty = () => ACTIONS.map(() => '')

  it('with no code entered, no row is in conflict', () => {
    expect(conflicts(ACTIONS, empty(), t)).toEqual(ACTIONS.map(() => null))
  })

  it('returns one entry per action, whatever the length of the received array', () => {
    // The result's length follows `ACTIONS`, never the input's: a too-short
    // array just means empty fields…
    expect(conflicts(ACTIONS, [], t)).toHaveLength(ACTIONS.length)
    // … and a too-long array does not make the function throw. Codes
    // beyond `ACTIONS` are ignored: they can no longer be reported as a
    // conflict under a nonexistent action key.
    expect(conflicts(ACTIONS, [...empty(), '9', '9'], t)).toEqual(ACTIONS.map(() => null))
  })

  it('all-distinct codes produce no conflict', () => {
    const codes = empty()
    codes[0] = '1'
    codes[1] = '2'
    codes[2] = '3'
    expect(conflicts(ACTIONS, codes, t)).toEqual(ACTIONS.map(() => null))
  })

  it('the same code on two actions puts both rows in conflict, each naming the other', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const codes = empty()
    codes[iMute] = '42'
    codes[iPower] = '42'
    const res = conflicts(ACTIONS, codes, t)
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
    const res = conflicts(ACTIONS, codes, t)
    expect(res[a]).toEqual({ code: 7, others: [ACTIONS[b]!.key, ACTIONS[c]!.key] })
    expect(res[b]).toEqual({ code: 7, others: [ACTIONS[a]!.key, ACTIONS[c]!.key] })
    expect(res[c]).toEqual({ code: 7, others: [ACTIONS[a]!.key, ACTIONS[b]!.key] })
  })

  it('a duplicate internal to the field names no other action', () => {
    const i = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const codes = empty()
    codes[i] = '115, 115'
    expect(conflicts(ACTIONS, codes, t)[i]).toEqual({ code: 115, others: [] })
  })

  it('on a multi-code field, reports the second code when only it is in cross-row conflict', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const codes = empty()
    codes[iMute] = '3, 8'
    codes[iPower] = '8'
    expect(conflicts(ACTIONS, codes, t)[iMute]).toEqual({ code: 8, others: ['act_power'] })
  })

  it('when the fields first code is an internal duplicate and the second is in cross-row conflict, reports the first (field order)', () => {
    const iMute = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const iPower = ACTIONS.findIndex((a) => a.key === 'act_power')
    const codes = empty()
    codes[iMute] = '5, 5, 8'
    codes[iPower] = '8'
    expect(conflicts(ACTIONS, codes, t)[iMute]).toEqual({ code: 5, others: [] })
  })

  it('ignores spaces and non-numeric entries, reports only the duplicate', () => {
    const i = ACTIONS.findIndex((a) => a.key === 'act_mute')
    const codes = empty()
    codes[i] = ' 9 , x , 9 '
    expect(conflicts(ACTIONS, codes, t)[i]).toEqual({ code: 9, others: [] })
  })

  it('names the other rows by label, not by key', () => {
    const tStop = (k: string) => (k === 'act_stop' ? 'Arrêt' : k)
    const rows: Row[] = [
      { key: 'act_stop', cmd: { cmd: 'Stop' } },
      { label: 'radio', cmd: { cmd: 'SelectSource', arg: 'radio' } },
    ]
    const out = conflicts(rows, ['42', '42'], tStop)
    expect(out[0]).toEqual({ code: 42, others: ['radio'] })
    expect(out[1]).toEqual({ code: 42, others: ['Arrêt'] })
  })
})

describe('sourceRows', () => {
  // The real catalogue keys (`act_select_source`, `act_select_source_unknown`)
  // only land with task 4. This stub renders the `{source}` placeholder
  // verbatim regardless of the key it is asked to translate, so composing it
  // already exercises `sourceRows`'s final shape (it takes `t`, same as every
  // other row-producing function here) without depending on catalogue
  // content that does not exist yet.
  const t = (_key: string) => '{source}'

  it('gives one row per known source, in catalogue order', () => {
    const rows = sourceRows(['cd', 'files', 'radio'], { devices: [] }, 'kbd', t)
    expect(rows.map((r) => r.label)).toEqual(['cd', 'files', 'radio'])
    expect(rows[0]!.cmd).toEqual({ cmd: 'SelectSource', arg: 'cd' })
  })

  it('keeps a row for a source that is bound but no longer installed', () => {
    // Dropping the row would drop the binding at the next save, in silence.
    const table: BindingTable = {
      devices: [{ name: 'kbd', bindings: [{ code: 77, cmd: 'SelectSource', arg: 'tape' }] }],
    }
    const rows = sourceRows(['radio'], table, 'kbd', t)
    expect(rows.map((r) => r.label)).toEqual(['radio', 'tape'])
  })

  it('ignores a stale binding belonging to another device', () => {
    const table: BindingTable = {
      devices: [{ name: 'other', bindings: [{ code: 77, cmd: 'SelectSource', arg: 'tape' }] }],
    }
    expect(sourceRows(['radio'], table, 'kbd', t).map((r) => r.label)).toEqual(['radio'])
  })
})

describe('sanitiseDeviceName', () => {
  it('reduces the name to a safe file identifier', () => {
    expect(sanitiseDeviceName('Media Center Ed. 3/4')).toBe('Media_Center_Ed_3_4')
    expect(sanitiseDeviceName('../../etc/passwd')).toBe('_etc_passwd')
  })
})
