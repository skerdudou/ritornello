export interface Command { cmd: string; arg?: number }
export interface Binding extends Command { code: number }
export interface DeviceBindings { name: string; bindings: Binding[] }
export interface BindingTable { devices: DeviceBindings[] }

// Les 23 actions, dans l'ordre de l'ancienne page (moins les deux entrees
// "preselection suivante/precedente", fusionnees sur `act_next`/`act_prev` :
// meme commande de protocole, interpretee par la source active - preselection
// pour la radio, piste pour le cd). Le libelle est traduit par le catalogue
// du plugin (cle `key`), la commande est un `ritornello_proto::Command`
// serialise (`cmd`/`arg`).
export const ACTIONS: Array<{ key: string; cmd: Command }> = [
  ...Array.from({ length: 9 }, (_, i) => ({
    key: `act_select_${i + 1}`,
    cmd: { cmd: 'Select', arg: i + 1 },
  })),
  // La touche 0 et +10 de la télécommande : 0 vaut « décalage + 0 » (10, 20…)
  // et +10 cumule le décalage tenu par le cœur.
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

const memeCmd = (a: Command, b: Command) => a.cmd === b.cmd && (a.arg ?? null) === (b.arg ?? null)

export function codesFor(table: BindingTable, device: string, cmd: Command): string {
  const d = table.devices.find((x) => x.name === device)
  if (!d) return ''
  return d.bindings.filter((b) => memeCmd(b, cmd)).map((b) => b.code).join(', ')
}

// Reconstruit la table complete : les autres peripheriques sont preserves
// tels quels, seul le peripherique courant est reecrit depuis le tableau.
// `codes` est indexe comme `ACTIONS`.
export function collect(table: BindingTable, device: string, codes: string[]): BindingTable {
  const devices = table.devices.filter((d) => d.name !== device)
  const bindings: Binding[] = []
  ACTIONS.forEach((a, i) => {
    const brut = (codes[i] ?? '').trim()
    if (!brut) return
    for (const part of brut.split(',')) {
      const code = Number.parseInt(part.trim(), 10)
      if (!Number.isNaN(code)) bindings.push({ code, ...a.cmd })
    }
  })
  if (device) devices.push({ name: device, bindings })
  return { devices }
}

// Serialisation TOML en miroir du format lu par `presets::parse_preset`
// (crates/ritornello-plugin-generic-input/src/presets.rs) : un bloc
// `[[bindings]]` par binding, `arg` seulement s'il est present. Toute
// evolution du format cote Rust doit etre repercutee ici, sous peine de
// fichiers exportes que le serveur refuserait.
export function presetToml(bindings: Binding[]): string {
  return bindings
    .map((b) => {
      let bloc = `[[bindings]]\ncode = ${b.code}\ncmd = "${b.cmd}"\n`
      if (b.arg !== undefined && b.arg !== null) bloc += `arg = ${b.arg}\n`
      return bloc
    })
    .join('\n')
}

export function sanitiseDeviceName(name: string): string {
  return name.replace(/[^a-zA-Z0-9_-]+/g, '_')
}
