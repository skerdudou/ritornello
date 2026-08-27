import type { Command, PlayerPayload } from '../types'

export interface RemoteCommand {
  key: string
  cmd: Command
}

// La charge utile est un `ritornello_proto::Command` serialise : c'est le meme
// canal que celui alimente par les plugins Input, donc aucune logique metier
// n'est dupliquee ici.
//
// Huit commandes sur la page, sur les dix-sept du protocole : les ±10 s et le
// volume pas a pas n'ont plus de touche web (voir `REMOTE_TRANSPORT`).

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
 * Le muet, a part lui aussi : c'est une bascule, pas un cran sur l'echelle du
 * volume, et il vit sur l'icone du haut-parleur au bout du curseur — la ou on
 * cherche le son.
 */
export const REMOTE_MUTE: RemoteCommand = { key: 'remote_mute', cmd: { cmd: 'Mute' } }

/**
 * Le transport : |◀ ▶ ▶| — precedent et suivant **adjacents** a la lecture,
 * qui est le seul bouton plein. C'est l'ordre des telecommandes hi-fi, de VLC
 * et des lecteurs de bureau : changer de piste est le geste frequent.
 *
 * Plus de « reculer / avancer de 10 s » : decide au chantier refonte, au vu de
 * VLC, Deezer et WMP qui n'en ont pas — c'est la barre d'avancement qui fait
 * ce travail (voir `BarreProgression`). `SeekBackward`/`SeekForward` restent
 * dans le protocole et sur la telecommande physique.
 *
 * Plus de « volume − / + » non plus : le volume est un curseur (`Volume.vue`),
 * pilote au clavier par fleches et Page ↑/↓, ce qui couvre l'accessibilite
 * que les deux touches auraient apportee. Elles restent le geste de la
 * telecommande physique, avec son appui maintenu.
 */
export const REMOTE_TRANSPORT: RemoteCommand[] = [
  { key: 'remote_prev', cmd: { cmd: 'Prev' } },
  { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'remote_next', cmd: { cmd: 'Next' } },
]

/**
 * En retrait du transport : l'arret, et l'ejection quand la source a un tiroir.
 */
export const REMOTE_TRANSPORT_SECONDAIRE: RemoteCommand[] = [
  { key: 'remote_stop', cmd: { cmd: 'Stop' } },
  { key: 'remote_eject', cmd: { cmd: 'Eject' } },
]

/**
 * Toutes les commandes de la page : sert au garde-fou i18n
 * (`i18nKeysUsed.test.ts`) et a verrouiller le compte de huit.
 */
export const REMOTE_COMMANDS: RemoteCommand[] = [
  REMOTE_POWER,
  REMOTE_SOURCE,
  REMOTE_MUTE,
  ...REMOTE_TRANSPORT,
  ...REMOTE_TRANSPORT_SECONDAIRE,
]

/**
 * Une commande que l'appareil ignorerait dans l'état courant : son bouton est
 * grisé plutôt qu'offert.
 *
 * Une seule règle désormais : en **veille**, le cœur retourne sans rien faire
 * sur tout ce qui n'est pas `Power` (première ligne de `handle_command`),
 * grille des présélections comprise. Le déplacement n'a plus de touche (c'est
 * la barre qui se rend inerte, sur `seekable`), et l'éjection se **masque**
 * plutôt que de se griser — voir `masquee`.
 *
 * Un état pas encore reçu (`null`) ne grise rien : la télécommande s'ouvre
 * utilisable, et la trame corrige aussitôt.
 */
export function indisponible(nom: string, etat: PlayerPayload | null): boolean {
  if (!etat) return false
  return etat.standby && nom !== 'Power'
}

/**
 * Une commande qui n'a pas lieu d'être sur cette source : son bouton n'est pas
 * rendu du tout.
 *
 * Seul `Eject` est concerné. `can_eject` est une capacité que le greffon source
 * déclare **pour lui-même** (`SourcePlugin::can_eject` du sdk) : le lecteur de
 * cd la déclare qu'il y ait un disque ou non, la radio ne la déclare pas.
 * Masquer sur cette base ne cache donc jamais un lecteur qui existe — au
 * contraire d'un grisage, qui affirmait une touche là où il n'y a pas de
 * tiroir. Avant la première trame on ne sait pas : rien n'est rendu, et la
 * trame corrige.
 */
export function masquee(nom: string, etat: PlayerPayload | null): boolean {
  return nom === 'Eject' && !(etat?.can_eject ?? false)
}
