import type { Command, PlayerPayload } from '../types'

export interface RemoteCommand {
  key: string
  cmd: Command
}

// La charge utile est un `ritornello_proto::Command` serialise : c'est le meme
// canal que celui alimente par les plugins Input, donc aucune logique metier
// n'est dupliquee ici.
//
// Douze commandes simples en tout — les deux entrees « preselection +/- » de
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
 * Le changement de source, a part elle aussi, et voisine de la veille dans le
 * coin de la carte.
 *
 * Meme raison qu'elle : ce n'est pas une commande de lecture mais un choix
 * portant sur l'appareil entier — ce qui joue change de nature, pas de piste.
 * Elle etait en fond de grille, dans la rangee « appareil », ou elle se lisait
 * comme un cran de plus apres le volume. La sortir la met au niveau de la
 * decision qu'elle represente, et laisse la grille aux seules commandes de la
 * lecture en cours.
 *
 * Elle garde son grisage, a la difference de la veille : en veille, le coeur
 * retourne sans rien faire sur tout ce qui n'est pas `Power` (voir
 * `indisponible`). Le bouton mentirait autrement.
 */
export const REMOTE_SOURCE: RemoteCommand = { key: 'remote_source', cmd: { cmd: 'SourceCycle' } }

/**
 * Les autres commandes, **groupees par ligne** dans l'ordre voulu par le
 * proprietaire : transport, changement de contenu, son, puis tiroir.
 *
 * Dans chaque rangee, l'ordre suit le sens du geste et non celui du protocole :
 * « precedent » avant « suivant », « moins » avant « plus », comme sur la
 * facade d'un ampli ou la reglette d'un lecteur. L'inverse — l'ordre dans
 * lequel ces commandes se sont trouvees ecrites d'abord — obligeait a lire les
 * libelles pour viser, alors que la position suffit quand elle va dans le sens
 * attendu. `remote_mute` reste en bout de sa rangee : ce n'est pas un cran de
 * plus sur l'echelle du volume, c'est une bascule.
 *
 * Le groupement est ici et non dans le gabarit : c'est une donnee, et la vue se
 * contente de la parcourir.
 */
export const REMOTE_ROWS: RemoteCommand[][] = [
  [
    { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
    { key: 'remote_stop', cmd: { cmd: 'Stop' } },
    { key: 'remote_seek_back', cmd: { cmd: 'SeekBackward' } },
    { key: 'remote_seek_forward', cmd: { cmd: 'SeekForward' } },
  ],
  [
    { key: 'remote_prev', cmd: { cmd: 'Prev' } },
    { key: 'remote_next', cmd: { cmd: 'Next' } },
  ],
  [
    { key: 'remote_vol_down', cmd: { cmd: 'VolumeDown' } },
    { key: 'remote_vol_up', cmd: { cmd: 'VolumeUp' } },
    { key: 'remote_mute', cmd: { cmd: 'Mute' } },
  ],
  [{ key: 'remote_eject', cmd: { cmd: 'Eject' } }],
]

/**
 * Toutes les commandes, veille et source comprises : sert au garde-fou qui
 * verifie que chaque cle de traduction employee existe bien dans le catalogue,
 * et a verrouiller le compte de douze. Les deux commandes hors grille y sont
 * nommees explicitement — c'est ce qui fait que les sortir de `REMOTE_ROWS` ne
 * les fait pas disparaitre du garde-fou.
 */
export const REMOTE_COMMANDS: RemoteCommand[] = [
  REMOTE_POWER,
  REMOTE_SOURCE,
  ...REMOTE_ROWS.flat(),
]

/**
 * Une commande que l'appareil ignorerait dans l'état courant : son bouton doit
 * être grisé plutôt qu'offert.
 *
 * Trois règles, et seulement celles que la charge utile de `/api/player`
 * permet d'établir :
 *
 * - en **veille**, le cœur retourne sans rien faire sur tout ce qui n'est pas
 *   `Power` (c'est la première ligne de `handle_command`), grille des
 *   présélections comprise. Ces boutons mentaient : la requête partait, le
 *   serveur répondait 204, et rien ne se passait.
 * - un contenu **non déplaçable** ignore les deux touches de déplacement. C'est
 *   le même `seekable` qui rend la barre de progression cliquable : les deux
 *   endroits de la page doivent dire la même chose d'un direct qu'on ne
 *   rembobine pas.
 * - une source **sans tiroir** ignore `Eject`. La source le déclare
 *   elle-même (`can_eject`, voir `SourcePlugin::can_eject` du sdk) : la page
 *   ne compare pas `source` à `'cd'`, un nom de plugin venant de
 *   `plugins.toml` et pouvant changer sans que rien ici ne s'en aperçoive.
 *
 * Le reste reste actif, faute de savoir : rien dans la charge utile ne dit si
 * quelque chose joue, donc `PlayPause` et `Stop` restent offerts. Un bouton
 * grisé **affirme** que l'action n'existe pas ; le griser sur une supposition
 * serait pire qu'un bouton sans effet.
 *
 * Un état pas encore reçu (`null` — la fraction de seconde avant la première
 * trame) ne grise rien : la télécommande s'ouvre utilisable, et la trame
 * corrige aussitôt.
 */
export function indisponible(nom: string, etat: PlayerPayload | null): boolean {
  if (!etat) return false
  if (etat.standby) return nom !== 'Power'
  if (nom === 'Eject') return !etat.can_eject
  return (nom === 'SeekForward' || nom === 'SeekBackward') && !etat.seekable
}
