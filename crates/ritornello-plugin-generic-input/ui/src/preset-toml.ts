export interface Command { cmd: string; arg?: number }
export interface Binding extends Command { code: number }
export interface DeviceBindings { name: string; bindings: Binding[] }
export interface BindingTable { devices: DeviceBindings[] }

// Les 23 actions, dans l'order de l'ancienne page (moins les deux entrees
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

const sameCmd = (a: Command, b: Command) => a.cmd === b.cmd && (a.arg ?? null) === (b.arg ?? null)

export function codesFor(table: BindingTable, device: string, cmd: Command): string {
  const d = table.devices.find((x) => x.name === device)
  if (!d) return ''
  return d.bindings.filter((b) => sameCmd(b, cmd)).map((b) => b.code).join(', ')
}

// Extrait les codes d'un champ : `trim`, decoupage sur la virgule, chaque
// partie passee a `Number.parseInt`, les non-numeriques ignores. Partagee par
// `collect` (qui en fait des `Binding`), `conflicts` (qui compare les nombres
// bruts) et l'ajout d'un code capte par apprentissage (`applyCode`, dans
// `InputAdmin.vue`, qui verifie si le code est deja la) : ces usages doivent
// rester en accord sur ce qui compte comme un code valide, sous peine de
// laisser la validation a chaud dire « aucun conflit » sur une table que le
// serveur refuserait a l'enregistrement.
export function parseField(brut: string): number[] {
  const trimmed = brut.trim()
  if (!trimmed) return []
  return trimmed
    .split(',')
    .map((part) => Number.parseInt(part.trim(), 10))
    .filter((code) => !Number.isNaN(code))
}

// Reconstruit la table complete : les autres peripheriques sont preserves
// tels quels, seul le peripherique courant est reecrit depuis le tableau.
// `codes` est indexe comme `ACTIONS`.
export function collect(table: BindingTable, device: string, codes: string[]): BindingTable {
  const devices = table.devices.filter((d) => d.name !== device)
  const bindings: Binding[] = []
  ACTIONS.forEach((a, i) => {
    for (const code of parseField(codes[i] ?? '')) bindings.push({ code, ...a.cmd })
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

export interface Conflict {
  /** Le code fautif. */
  code: number
  /** Clés i18n des *autres* actions portant ce code, dans l'order d'`ACTIONS`. Vide si le doublon est interne au champ. */
  autres: string[]
}

// Detecte, pour chaque action affichee, le premier code fautif de son champ :
// soit un code deja porte par une autre action (exactement ce que le serveur
// refuserait a l'enregistrement, `duplicate_code`, mais visible avant), soit
// un code saisi plusieurs fois dans le meme champ. Un seul conflit par ligne,
// choisi dans l'order du champ, pour qu'il n'y ait jamais qu'un message a
// afficher sous un champ donne.
export function conflicts(codes: string[]): Array<Conflict | null> {
  // Le journey est celui d'`ACTIONS`, pas celui de `codes` : le resultat a
  // toujours une entree par action, quelle que soit la longueur du tableau
  // recu (`codes` est indexe comme `ACTIONS`, un tableau plus court signifie
  // simplement des champs vides). Et chaque ligne porte sa propre cle i18n,
  // ce qui remplace la recherche `ACTIONS[j]` d'un indice venu du tableau
  // d'entree -- laquelle rendait `undefined`, donc levait une `TypeError`,
  // pour tout appelant passant plus de codes qu'il n'existe d'actions.
  const lignes = ACTIONS.map((a, i) => ({ cle: a.key, codes: parseField(codes[i] ?? '') }))

  // Pour chaque code, les lignes qui le portent au moins une fois — sert a
  // reperer les doublons inter-actions sans reparcourir toute la table pour
  // chaque code candidat.
  const lignesParCode = new Map<number, typeof lignes>()
  for (const ligne of lignes) {
    for (const code of new Set(ligne.codes)) {
      const portees = lignesParCode.get(code) ?? []
      portees.push(ligne)
      lignesParCode.set(code, portees)
    }
  }

  return lignes.map((ligne) => {
    for (const code of ligne.codes) {
      const autresLignes = (lignesParCode.get(code) ?? []).filter((l) => l !== ligne)
      if (autresLignes.length > 0) {
        return { code, autres: autresLignes.map((l) => l.cle) }
      }
      const occurrences = ligne.codes.filter((c) => c === code).length
      if (occurrences >= 2) {
        return { code, autres: [] }
      }
    }
    return null
  })
}

export function sanitiseDeviceName(name: string): string {
  return name.replace(/[^a-zA-Z0-9_-]+/g, '_')
}
