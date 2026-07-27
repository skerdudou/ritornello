import type { Command } from '../types'

export interface RemoteCommand {
  key: string
  cmd: Command
}

// La charge utile est un `ritornello_proto::Command` serialise : c'est le meme
// canal que celui alimente par les plugins Input, donc aucune logique metier
// n'est dupliquee ici.
//
// Dix commandes simples en tout — les deux entrees « preselection +/- » de
// l'ancienne page ont ete fusionnees sur `Next`/`Prev` : meme commande de
// protocole, interpretee par la source active (preselection pour la radio,
// piste pour le cd).

/**
 * La veille, a part : elle occupe le coin de la carte et non la grille des
 * commandes. C'est l'action la plus consequente du lot, et la seule qui agisse
 * sur l'appareil entier plutot que sur la lecture.
 */
export const REMOTE_POWER: RemoteCommand = { key: 'remote_power', cmd: { cmd: 'Power' } }

/**
 * Les autres commandes, **groupees par ligne** dans l'ordre voulu par le
 * proprietaire : transport, changement de contenu, son, puis appareil.
 *
 * Le groupement est ici et non dans le gabarit : c'est une donnee, et la vue se
 * contente de la parcourir.
 */
export const REMOTE_ROWS: RemoteCommand[][] = [
  [
    { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
    { key: 'remote_stop', cmd: { cmd: 'Stop' } },
  ],
  [
    { key: 'remote_next', cmd: { cmd: 'Next' } },
    { key: 'remote_prev', cmd: { cmd: 'Prev' } },
  ],
  [
    { key: 'remote_vol_up', cmd: { cmd: 'VolumeUp' } },
    { key: 'remote_vol_down', cmd: { cmd: 'VolumeDown' } },
    { key: 'remote_mute', cmd: { cmd: 'Mute' } },
  ],
  [
    { key: 'remote_source', cmd: { cmd: 'SourceCycle' } },
    { key: 'remote_eject', cmd: { cmd: 'Eject' } },
  ],
]

/**
 * Toutes les commandes, veille comprise : sert au garde-fou qui verifie que
 * chaque cle de traduction employee existe bien dans le catalogue, et a
 * verrouiller le compte de dix.
 */
export const REMOTE_COMMANDS: RemoteCommand[] = [REMOTE_POWER, ...REMOTE_ROWS.flat()]
