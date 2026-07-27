import type { Command } from '../types'

// Les 10 commandes simples, dans l'ordre exact de l'ancienne page de statut
// (moins les deux entrees "preselection +/-", fusionnees sur `Next`/`Prev` :
// meme commande de protocole, interpretee par la source active - preselection
// pour la radio, piste pour le cd). La charge utile est un
// `ritornello_proto::Command` serialise : c'est le meme canal que celui
// alimente par les plugins Input, donc aucune logique metier n'est dupliquee
// ici.
export const REMOTE_COMMANDS: Array<{ key: string; cmd: Command }> = [
  { key: 'remote_vol_up', cmd: { cmd: 'VolumeUp' } },
  { key: 'remote_vol_down', cmd: { cmd: 'VolumeDown' } },
  { key: 'remote_mute', cmd: { cmd: 'Mute' } },
  { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'remote_stop', cmd: { cmd: 'Stop' } },
  { key: 'remote_next', cmd: { cmd: 'Next' } },
  { key: 'remote_prev', cmd: { cmd: 'Prev' } },
  { key: 'remote_eject', cmd: { cmd: 'Eject' } },
  { key: 'remote_source', cmd: { cmd: 'SourceCycle' } },
  { key: 'remote_power', cmd: { cmd: 'Power' } },
]
