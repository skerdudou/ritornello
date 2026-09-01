import type { Command, PlayerPayload } from '../types'

export interface RemoteCommand {
  key: string
  cmd: Command
}

// The payload is a serialized `ritornello_proto::Command`: it is the same
// channel as the one fed by the Input plugins, so no business logic is
// duplicated here.
//
// Eight commands on the page, out of the seventeen of the protocol: the ±10 s
// and the step-by-step volume no longer have a web key (see `REMOTE_TRANSPORT`).

/**
 * Standby, set apart: it occupies the corner of the card and not the command
 * grid. It is the most consequential action of the lot, and the only one that
 * acts on the whole device rather than on playback.
 */
export const REMOTE_POWER: RemoteCommand = { key: 'remote_power', cmd: { cmd: 'Power' } }

/**
 * The source change, set apart as well, and next to standby in the corner of
 * the card.
 *
 * Same reason as standby: it is not a playback command but a choice bearing
 * on the whole device — what plays changes in nature, not in track. It used
 * to be at the bottom of the grid, in the "device" row, where it read as one
 * more notch after the volume. Taking it out puts it at the level of the
 * decision it represents, and leaves the grid to the commands of the current
 * playback only.
 *
 * It keeps its greying, unlike standby: in standby, the core returns without
 * doing anything on everything that is not `Power` (see `unavailable`). The
 * button would lie otherwise.
 */
export const REMOTE_SOURCE: RemoteCommand = { key: 'remote_source', cmd: { cmd: 'SourceCycle' } }

/**
 * Mute, set apart as well: it is a toggle, not a notch on the volume scale,
 * and it lives on the speaker icon at the end of the slider — where one looks
 * for the sound.
 */
export const REMOTE_MUTE: RemoteCommand = { key: 'remote_mute', cmd: { cmd: 'Mute' } }

/**
 * Transport: |◀ ▶ ▶| — previous and next **adjacent** to play, which is the
 * only filled button. That is the order of hi-fi remotes, VLC and desktop
 * players: changing track is the frequent gesture.
 *
 * No more "back / forward 10 s": decided during the redesign, in view of VLC,
 * Deezer and WMP which do not have it — the progress bar does that job (see
 * `ProgressBar`). `SeekBackward`/`SeekForward` remain in the protocol and on
 * the physical remote.
 *
 * No more "volume − / +" either: the volume is a slider (`Volume.vue`), driven
 * from the keyboard by arrows and Page ↑/↓, which covers the accessibility
 * the two keys would have brought. They remain the gesture of the physical
 * remote, with its held press.
 */
export const REMOTE_TRANSPORT: RemoteCommand[] = [
  { key: 'remote_prev', cmd: { cmd: 'Prev' } },
  { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'remote_next', cmd: { cmd: 'Next' } },
]

/**
 * Behind the transport: stop, and eject when the source has a tray.
 */
export const REMOTE_TRANSPORT_SECONDARY: RemoteCommand[] = [
  { key: 'remote_stop', cmd: { cmd: 'Stop' } },
  { key: 'remote_eject', cmd: { cmd: 'Eject' } },
]

/**
 * All the commands of the page: used by the i18n safeguard
 * (`i18nKeysUsed.test.ts`) and to lock the count of eight.
 */
export const REMOTE_COMMANDS: RemoteCommand[] = [
  REMOTE_POWER,
  REMOTE_SOURCE,
  REMOTE_MUTE,
  ...REMOTE_TRANSPORT,
  ...REMOTE_TRANSPORT_SECONDARY,
]

/**
 * A command the device would ignore in the current state: its button is
 * greyed rather than offered.
 *
 * A single rule now: in **standby**, the core returns without doing anything
 * on everything that is not `Power` (first line of `handle_command`), preset
 * grid included. Seeking no longer has a key (it is the bar that goes inert,
 * on `seekable`), and eject is **hidden** rather than greyed — see `hidden`.
 *
 * A state not yet received (`null`) greys nothing: the remote opens usable,
 * and the frame corrects at once.
 */
export function unavailable(name: string, state: PlayerPayload | null): boolean {
  if (!state) return false
  return state.standby && name !== 'Power'
}

/**
 * A command that has no place on this source: its button is not rendered at
 * all.
 *
 * Only `Eject` is concerned. `can_eject` is a capability the source plugin
 * declares **for itself** (`SourcePlugin::can_eject` of the sdk): the cd
 * player declares it whether there is a disc or not, the radio does not
 * declare it. Hiding on that basis therefore never hides a player that exists
 * — unlike greying, which asserted a key where there is no tray. Before the
 * first frame we do not know: nothing is rendered, and the frame corrects.
 */
export function hidden(name: string, state: PlayerPayload | null): boolean {
  return name === 'Eject' && !(state?.can_eject ?? false)
}
