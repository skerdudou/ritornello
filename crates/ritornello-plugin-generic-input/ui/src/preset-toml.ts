export interface Command { cmd: string; arg?: number }
export interface Binding extends Command { code: number }
export interface DeviceBindings { name: string; bindings: Binding[] }
export interface BindingTable { devices: DeviceBindings[] }

// The 23 actions, in the old page's order (minus the two "next/previous
// preset" entries, merged into `act_next`/`act_prev`: same protocol
// command, interpreted by the active source - preset for radio, track for
// cd). The label is translated by the plugin's catalog (`key`), the
// command is a serialized `ritornello_proto::Command` (`cmd`/`arg`).
export const ACTIONS: Array<{ key: string; cmd: Command }> = [
  ...Array.from({ length: 9 }, (_, i) => ({
    key: `act_select_${i + 1}`,
    cmd: { cmd: 'Select', arg: i + 1 },
  })),
  // The remote's 0 key and +10: 0 means "offset + 0" (10, 20…) and +10
  // accumulates the offset held by the core.
  { key: 'act_select_0', cmd: { cmd: 'Select', arg: 0 } },
  { key: 'act_plus10', cmd: { cmd: 'Plus10' } },
  { key: 'act_volume_up', cmd: { cmd: 'VolumeUp' } },
  { key: 'act_volume_down', cmd: { cmd: 'VolumeDown' } },
  { key: 'act_mute', cmd: { cmd: 'Mute' } },
  { key: 'act_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'act_stop', cmd: { cmd: 'Stop' } },
  { key: 'act_seek_back', cmd: { cmd: 'SeekBackward' } },
  { key: 'act_seek_forward', cmd: { cmd: 'SeekForward' } },
  { key: 'act_next', cmd: { cmd: 'Next' } },
  { key: 'act_prev', cmd: { cmd: 'Prev' } },
  { key: 'act_eject', cmd: { cmd: 'Eject' } },
  { key: 'act_source_cycle', cmd: { cmd: 'SourceCycle' } },
  { key: 'act_power', cmd: { cmd: 'Power' } },
]

const sameCmd = (a: Command, b: Command) => a.cmd === b.cmd && (a.arg ?? null) === (b.arg ?? null)

export function codesFor(table: BindingTable, device: string, cmd: Command): string {
  const d = table.devices.find((x) => x.name === device)
  if (!d) return ''
  return d.bindings.filter((b) => sameCmd(b, cmd)).map((b) => b.code).join(', ')
}

// Extracts the codes from a field: `trim`, split on comma, each part passed
// to `Number.parseInt`, non-numeric ones ignored. Shared by `collect`
// (which turns them into `Binding`s), `conflicts` (which compares the raw
// numbers) and the addition of a code captured via learning (`applyCode`,
// in `InputAdmin.vue`, which checks whether the code is already there):
// these usages must stay in agreement on what counts as a valid code, or
// live validation would say "no conflict" on a table the server would
// reject at save time.
export function parseField(raw: string): number[] {
  const trimmed = raw.trim()
  if (!trimmed) return []
  return trimmed
    .split(',')
    .map((part) => Number.parseInt(part.trim(), 10))
    .filter((code) => !Number.isNaN(code))
}

// Rebuilds the complete table: the other devices are preserved as-is, only
// the current device is rewritten from the array. `codes` is indexed like
// `ACTIONS`.
export function collect(table: BindingTable, device: string, codes: string[]): BindingTable {
  const devices = table.devices.filter((d) => d.name !== device)
  const bindings: Binding[] = []
  ACTIONS.forEach((a, i) => {
    for (const code of parseField(codes[i] ?? '')) bindings.push({ code, ...a.cmd })
  })
  if (device) devices.push({ name: device, bindings })
  return { devices }
}

// TOML serialization mirroring the format read by `presets::parse_preset`
// (crates/ritornello-plugin-generic-input/src/presets.rs): one
// `[[bindings]]` block per binding, `arg` only if present. Any evolution of
// the format on the Rust side must be mirrored here, or exported files
// would be rejected by the server.
export function presetToml(bindings: Binding[]): string {
  return bindings
    .map((b) => {
      let block = `[[bindings]]\ncode = ${b.code}\ncmd = "${b.cmd}"\n`
      if (b.arg !== undefined && b.arg !== null) block += `arg = ${b.arg}\n`
      return block
    })
    .join('\n')
}

export interface Conflict {
  /** The faulty code. */
  code: number
  /** i18n keys of the *other* actions carrying this code, in `ACTIONS`'s order. Empty if the duplicate is internal to the field. */
  others: string[]
}

// Detects, for each displayed action, the first faulty code in its field:
// either a code already carried by another action (exactly what the server
// would reject at save time, `duplicate_code`, but visible beforehand), or
// a code entered several times in the same field. A single conflict per
// row, chosen in the field's order, so that there is never more than one
// message to display under a given field.
export function conflicts(codes: string[]): Array<Conflict | null> {
  // The traversal follows `ACTIONS`, not `codes`: the result always has one
  // entry per action, whatever the length of the received array (`codes`
  // is indexed like `ACTIONS`, a shorter array simply means empty fields).
  // And each row carries its own i18n key, which replaces looking up
  // `ACTIONS[j]` from an index coming from the input array -- which used to
  // yield `undefined`, and so throw a `TypeError`, for any caller passing
  // more codes than there are actions.
  const rows = ACTIONS.map((a, i) => ({ key: a.key, codes: parseField(codes[i] ?? '') }))

  // For each code, the rows that carry it at least once — used to spot
  // cross-action duplicates without rescanning the whole table for each
  // candidate code.
  const rowsByCode = new Map<number, typeof rows>()
  for (const row of rows) {
    for (const code of new Set(row.codes)) {
      const carriers = rowsByCode.get(code) ?? []
      carriers.push(row)
      rowsByCode.set(code, carriers)
    }
  }

  return rows.map((row) => {
    for (const code of row.codes) {
      const otherRows = (rowsByCode.get(code) ?? []).filter((r) => r !== row)
      if (otherRows.length > 0) {
        return { code, others: otherRows.map((r) => r.key) }
      }
      const occurrences = row.codes.filter((c) => c === code).length
      if (occurrences >= 2) {
        return { code, others: [] }
      }
    }
    return null
  })
}

export function sanitiseDeviceName(name: string): string {
  return name.replace(/[^a-zA-Z0-9_-]+/g, '_')
}
