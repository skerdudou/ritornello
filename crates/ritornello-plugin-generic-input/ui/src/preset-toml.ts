export interface Command { cmd: string; arg?: number | string | boolean }
export interface Binding extends Command { code: number }
export interface DeviceBindings { name: string; bindings: Binding[] }
export interface BindingTable { devices: DeviceBindings[] }

/**
 * One row of the bindings table. Either it names a catalogue key (the 23
 * fixed actions) or it carries an already-resolved label (a source
 * shortcut, whose label is the source's own name and which no catalogue
 * knows). Never both, and never neither.
 */
export interface Row { key?: string; label?: string; cmd: Command }

/** Resolves a row's displayed label: translates the key, or passes the already-resolved label through. */
export function rowLabel(row: Row, t: (k: string) => string): string {
  return row.key ? t(row.key) : (row.label ?? '')
}

// The 23 actions, in the old page's order (minus the two "next/previous
// preset" entries, merged into `act_next`/`act_prev`: same protocol
// command, interpreted by the active source - preset for radio, track for
// cd). The label is translated by the plugin's catalog (`key`), the
// command is a serialized `ritornello_proto::Command` (`cmd`/`arg`).
export const ACTIONS: Row[] = [
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

/**
 * The source shortcut rows, appended after the fixed actions: one per
 * source the core announces (`/api/presets`), in the catalogue's order —
 * which is the order of the "change source" key, so the page and the key
 * agree.
 *
 * Then one row per source **bound on this device but absent from the
 * catalogue**: the plugin providing it may have been uninstalled since, and
 * silently dropping its row would drop the binding at the next save without
 * telling the operator why their key stopped working.
 *
 * `t` composes the label itself (`act_select_source`/
 * `act_select_source_unknown`, each carrying a `{source}` placeholder)
 * rather than a plain key: a source row has no catalogue key of its own
 * (see `Row`), so the label must already be resolved by the time it is
 * built, unlike the fixed `ACTIONS`.
 */
export function sourceRows(
  sources: string[],
  table: BindingTable,
  device: string,
  t: (k: string) => string,
): Row[] {
  const label = (key: string, name: string) => t(key).replace('{source}', name)
  const known = sources.map((name) => ({
    label: label('act_select_source', name),
    cmd: { cmd: 'SelectSource', arg: name } as Command,
  }))
  const bound = table.devices.find((d) => d.name === device)?.bindings ?? []
  // A source can be bound on several codes; de-duplicated, or an
  // uninstalled source with two bindings would print its orphan row twice.
  const orphans = [
    ...new Set(
      bound
        .filter((b) => b.cmd === 'SelectSource' && typeof b.arg === 'string')
        .map((b) => b.arg as string)
        .filter((name) => !sources.includes(name)),
    ),
  ]
  return [
    ...known,
    ...orphans.map((name) => ({
      label: label('act_select_source_unknown', name),
      cmd: { cmd: 'SelectSource', arg: name } as Command,
    })),
  ]
}

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
// `rows`.
export function collect(table: BindingTable, device: string, rows: Row[], codes: string[]): BindingTable {
  const devices = table.devices.filter((d) => d.name !== device)
  const bindings: Binding[] = []
  rows.forEach((r, i) => {
    for (const code of parseField(codes[i] ?? '')) bindings.push({ code, ...r.cmd })
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
      if (b.arg !== undefined && b.arg !== null) {
        // TOML types: a string needs quotes, a number and a boolean are bare
        // words. Getting this wrong only shows on the export/re-import round
        // trip, which is why it is pinned by a test rather than by review.
        const value = typeof b.arg === 'string' ? JSON.stringify(b.arg) : String(b.arg)
        block += `arg = ${value}\n`
      }
      return block
    })
    .join('\n')
}

export interface Conflict {
  /** The faulty code. */
  code: number
  /** Labels of the *other* rows carrying this code, in `rows`'s order. Empty if the duplicate is internal to the field. */
  others: string[]
}

// Detects, for each displayed row, the first faulty code in its field:
// either a code already carried by another row (exactly what the server
// would reject at save time, `duplicate_code`, but visible beforehand), or
// a code entered several times in the same field. A single conflict per
// row, chosen in the field's order, so that there is never more than one
// message to display under a given field.
export function conflicts(rows: Row[], codes: string[], t: (k: string) => string): Array<Conflict | null> {
  // The traversal follows `rows`, not `codes`: the result always has one
  // entry per row, whatever the length of the received array (`codes` is
  // indexed like `rows`, a shorter array simply means empty fields). And
  // each entry carries its own resolved label, which replaces looking up
  // `rows[j]` from an index coming from the input array -- which used to
  // yield `undefined`, and so throw a `TypeError`, for any caller passing
  // more codes than there are rows.
  const entries = rows.map((r, i) => ({ label: rowLabel(r, t), codes: parseField(codes[i] ?? '') }))

  // For each code, the entries that carry it at least once — used to spot
  // cross-row duplicates without rescanning the whole table for each
  // candidate code.
  const entriesByCode = new Map<number, typeof entries>()
  for (const entry of entries) {
    for (const code of new Set(entry.codes)) {
      const carriers = entriesByCode.get(code) ?? []
      carriers.push(entry)
      entriesByCode.set(code, carriers)
    }
  }

  return entries.map((entry) => {
    for (const code of entry.codes) {
      const others = (entriesByCode.get(code) ?? []).filter((e) => e !== entry)
      if (others.length > 0) {
        return { code, others: others.map((e) => e.label) }
      }
      const occurrences = entry.codes.filter((c) => c === code).length
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
