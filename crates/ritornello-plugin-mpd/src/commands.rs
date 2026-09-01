//! What an MPD command becomes: a snapshot in, lines out. **No I/O, no
//! clock.**
//!
//! That purity is the point of the module, not an elegance: the mapping
//! between an MPD command and the device's facade is the first thing a
//! client sees, and it is also what is hardest to verify by eye. A function
//! that does nothing but choose can be tested line by line; the session
//! (Task 8) keeps everything that touches the socket to itself.
//!
//! Its caller is `session.rs`, which reads the lines and writes the
//! responses: it, and it alone, calls `handle`.

use crate::state::{Snapshot, Subsystem};
use crate::protocol::{ack, line, Ack};
use ritornello_proto::{Command, Playback, Preset, SourceCatalog};
use std::ops::Range;
use std::sync::Arc;

/// What handling a command asks the session to do.
///
/// The decision is **pure** and the application impure: this module chooses,
/// the session writes to the socket and pushes onto the channel. That is what
/// makes the mapping verifiable by unit test.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// These lines, then `OK` — written by the session, not by us: in a
    /// command list, a single `OK` closes the whole batch.
    Reply { lines: Vec<String>, cmds: Vec<Command> },
    /// An `ACK`, already formatted. In a list, it interrupts what follows.
    Reject(String),
    /// `idle`: wait for one of these subsystems.
    ///
    /// **The list may be empty**, and that does not mean "answer right
    /// away": a client that only named subsystems this plugin never emits
    /// (`idle database`) must wait forever. That is the correct MPD
    /// behavior — it asked to be told about a change that never happens.
    /// Task 8 must therefore not treat the empty list as an `OK`.
    Wait(Vec<Subsystem>),
    /// `noidle` received while not waiting: a bare `OK`.
    Cancel,
    /// A **binary** response: `albumart` and `readpicture`.
    ///
    /// A separate variant, not `lines`: the bytes of an image are not
    /// UTF-8, so they cannot travel in `Reply`'s `Vec<String>` — and above
    /// all they must not go through the session's text accumulator, which is
    /// what was found to amplify by a factor of 2048 on this very port. See
    /// `Binary`.
    Bytes(Binary),
    /// `close`: `OK`, then close the connection.
    Close,
    /// `binarylimit <N>`: the session remembers this chunk size for its
    /// binary responses, then answers `OK`.
    ///
    /// A separate variant because it is a fact about the **connection** and
    /// not about the device — the same reason the list state and the `idle`
    /// wait live in `session.rs`. The carried value is already clamped (see
    /// `binarylimit`), the session has nothing to re-check.
    BinaryLimit(usize),
}

/// A fully decided binary response: the textual header, the image, and the
/// window of this response within the image.
///
/// **The image is shared, the chunk is a range**: this module stays pure (no
/// I/O, no image allocation), the session only has to write. Cloning the
/// `Arc` is a counter increment, so building this variant **never** copies
/// the bytes, even for a 20 MiB image (`COVER_MAX_BYTES`); what the session
/// will write is bounded by `MAX_CHUNK` and by it alone. On the other hand
/// that clone **holds on to** this image generation until the write
/// finishes: see the product computed on `MAX_CHUNK`.
#[derive(Clone, PartialEq)]
pub struct Binary {
    /// `size: <total>`, and for `readpicture` `type: <mime>` — in that
    /// order, MPD's own.
    pub header: Vec<String>,
    /// The **whole** image, shared with the state (never copied).
    pub image: Arc<Vec<u8>>,
    /// The window to write. Always within the bounds of `image` and at most
    /// `MAX_CHUNK` bytes: `albumart` establishes it, and the session relies
    /// on it to index without checking.
    pub chunk: Range<usize>,
}

/// A hand-written `Debug`, for the same reason as `HeldCover`'s: the derived
/// one would print twenty mebibytes of image in a failing test's message.
impl std::fmt::Debug for Binary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binary")
            .field("header", &self.header)
            .field("image", &format_args!("{} o", self.image.len()))
            .field("chunk", &self.chunk)
            .finish()
    }
}

impl Outcome {
    /// A bare `OK`.
    pub fn ok() -> Self {
        Outcome::Reply { lines: Vec::new(), cmds: Vec::new() }
    }

    pub fn lines(lines: Vec<String>) -> Self {
        Outcome::Reply { lines, cmds: Vec::new() }
    }

    /// A bare `OK`, plus one command to emit toward the core.
    ///
    /// No caller before **Task 7**: no read-only command acts on the device,
    /// and that is a property a test of this module checks explicitly.
    pub fn acting(cmd: Command) -> Self {
        Outcome::Reply { lines: Vec::new(), cmds: vec![cmd] }
    }
}

/// The commands this server actually handles, and nothing else.
///
/// **The `commands` command is what keeps the plugin honest**: a correct
/// client reads there what exists and greys out the rest on its own. The
/// difference between "empty tabs" and "crashing tabs" hangs on this list,
/// so it must never promise more than `handle`'s `match`. A test walks the
/// list and checks that every name in it is actually handled.
///
/// Alphabetical order: clients get nothing out of it, but a gap shows.
pub const COMMANDS: &[&str] = &[
    "add",
    "addid",
    "albumart",
    "binarylimit",
    "clear",
    "close",
    "commands",
    "count",
    "currentsong",
    "decoders",
    "find",
    "getvol",
    "idle",
    "list",
    "listall",
    "listallinfo",
    "listfiles",
    "listplaylistinfo",
    "listplaylists",
    "load",
    "lsinfo",
    "next",
    "noidle",
    "notcommands",
    "outputs",
    "password",
    "pause",
    "ping",
    "play",
    "playid",
    "playlistinfo",
    "plchanges",
    "previous",
    "readpicture",
    "search",
    "seek",
    "seekcur",
    "seekid",
    "setvol",
    "stats",
    "status",
    "stop",
    "tagtypes",
    "urlhandlers",
    "volume",
];

/// One queue entry: its preset index (**sparse**, 1-based, the one
/// `Command::Select` expects) and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub index: u8,
    pub name: String,
}

/// The entries of a named preset list, as the sources catalog gives it. The
/// indices are copied **as-is**, sparse included: nothing here derives a rank
/// from an index.
fn named_entries(presets: &[Preset]) -> Vec<Entry> {
    presets.iter().map(|p| Entry { index: p.index, name: p.name.clone() }).collect()
}

/// The MPD queue: the presets of the active source.
///
/// **Two branches, and the order between them is the point.**
/// 1. The **real list**, when the sources catalog gives a non-empty one for
///    the active source. Its indices are the source's, possibly **sparse**:
///    `preset_count` is the *maximum* of the numbers, not their count, so
///    stations 1, 5 and 99 are legal, while MPD positions stay dense. The
///    mapping therefore goes through the **rank** in this list
///    (`position_to_index`), never through a subtraction of 1.
/// 2. The **synthesis**, otherwise: the plugin fabricates
///    `1..=preset_count`, and the sequence is then dense by construction
///    (`Pos = Id - 1`). That is the cd's case, which cannot enumerate — its
///    sources-catalog entry carries an empty list, which means "I only have
///    numbers" and not "I have nothing". Falling back on `preset_count` is
///    then the only way to see the twelve tracks of a disc.
///
/// `None` becomes **zero entries**, not the ten of the web UI's default
/// grid: that grid is a numeric keypad, not a list. Announcing ten entries
/// would make a client ask for ten things none of which exists.
pub fn queue(inst: &Snapshot) -> Vec<Entry> {
    let presets = inst.active_presets();
    if !presets.is_empty() {
        return named_entries(presets);
    }
    let n = inst.state.preset_count.unwrap_or(0);
    (1..=n).map(|i| Entry { index: i, name: i.to_string() }).collect()
}

/// The date MPD expects on every `listplaylists` entry, for lack of a real
/// one.
///
/// No date exists on the device side: a source is not a file, it has no
/// modification date nor anything resembling one, and fabricating one from
/// the current clock would make a client believe a list just changed every
/// time it re-reads it. A constant, then — and the epoch rather than an
/// arbitrary date, because it reads as "unknown".
///
/// **Emitted, not omitted**: the field is optional in the protocol's
/// documentation, but clients read it without guarding (libmpdclient sorts
/// its lists on it), and its absence trips them up. Emitting it costs one
/// line and will never lie, since it will never move.
const UNKNOWN_DATE: &str = "1970-01-01T00:00:00Z";

/// Handles an already-split command. `index` is its rank in a command list
/// (0 outside a list): it must travel all the way to the `ACK`, otherwise a
/// client cannot tell which of its commands failed.
/// `binary_limit` is the chunk size **this connection** accepts (see
/// `binarylimit`): a fact about the connection, known only to the session,
/// and entering nowhere but the two cover commands.
pub fn handle(
    inst: &Snapshot,
    index: usize,
    args: &[String],
    binary_limit: usize,
) -> Outcome {
    // Empty line: the session should not submit one, but this function is
    // total by construction rather than by convention — an `args[0]` on an
    // empty slice would be a panic, hence a dropped connection.
    let Some(cmd) = args.first() else {
        return Outcome::Reject(ack(Ack::Unknown, index, "", "unsupported"));
    };
    let remainder = &args[1..];
    match cmd.as_str() {
        "status" => Outcome::lines(status(inst)),
        "currentsong" => Outcome::lines(currentsong(inst)),
        "playlistinfo" => playlistinfo(inst, index, remainder),
        "plchanges" => plchanges(inst, index, remainder),
        // Every source of the sources catalog **is** a stored playlist: that
        // is the mapping that makes the device readable from an MPD client,
        // where "load the radio playlist" means "listen to the radio".
        "listplaylists" => Outcome::lines(listplaylists(inst)),
        // Queryable on any source, including one that is not playing: it is
        // a fact about a source, not about what is playing.
        "listplaylistinfo" => listplaylistinfo(inst, index, remainder),
        "commands" => Outcome::lines(COMMANDS.iter().map(|c| line("command", c)).collect()),
        // Its counterpart, which old clients ask for right after `commands`.
        // Empty, and that is the honest answer: `notcommands` lists what the
        // current password *forbids*, and there is no password here (see the
        // spec, § Network), so nothing is forbidden by permission. What does
        // not exist is simply absent from `commands`.
        "notcommands" => Outcome::ok(),
        // The only four tags `Track` carries. Announcing more would make the
        // client look for sort orders nothing feeds; forgetting one that
        // `currentsong` emits is the opposite defect, and it was there: a
        // client is entitled to read only the lines of the tags announced
        // here, so the year stayed invisible on its side. The list must
        // remain the exact mirror of what `currentsong` can emit.
        "tagtypes" => Outcome::lines(
            ["Artist", "Album", "Title", "Date"].iter().map(|t| line("tagtype", t)).collect(),
        ),
        // A single, always-active output: the device has one audio output,
        // which the admin page chooses. `enableoutput`/`disableoutput` are
        // refused, so nothing here is steerable — but a client that sees no
        // output at all displays "muted" and does not insist.
        "outputs" => Outcome::lines(vec![
            line("outputid", 0),
            line("outputname", "default"),
            line("outputenabled", 1),
        ]),
        "stats" => Outcome::lines(stats(inst)),
        // A bare `OK`, but **present**: a command unknown at connection time
        // can make a client give up before it draws a screen. No value to
        // give (there is no decoder plugin nor URL scheme to expose), and an
        // empty list is a well-formed answer.
        "decoders" | "urlhandlers" => Outcome::ok(),
        "ping" => Outcome::ok(),
        // Accepted without checking anything: the server has no password
        // (see the spec, § Network), and a client configured with one must
        // not be rejected for it. Without an argument too: there is nothing
        // to check, so nothing to refuse.
        "password" => Outcome::ok(),
        "close" => Outcome::Close,
        // `idle` does not *answer* here: this module picks the subsystems,
        // the session (Task 8) holds the wait and decides that an `idle`
        // inside a command list is illegal. Parsing the subsystem names,
        // though, is pure.
        "idle" => idle(index, remainder),
        "noidle" => Outcome::Cancel,
        // `play [POS]`: POS is the **rank** in the queue (0-based, the one
        // `Pos` publishes), never the preset index minus one — the two no
        // longer coincide as soon as a source enumerates a sparse list.
        // Without an argument it is not a selection but the Play key:
        // restart whatever was loaded.
        "play" => play(inst, index, remainder),
        // `playid <ID>`: the index as-is, but checked against the queue — an
        // `ID` within the maximum (`preset_count`) without being a real
        // entry of the sparse queue must refuse; a bound is not enough.
        "playid" => playid(inst, index, remainder),
        // Toggles without an argument; otherwise acts only if the optimistic
        // state differs from the target — that is what closes the race of a
        // client resending the same command twice. See `pause`.
        "pause" => pause(inst, index, remainder),
        "stop" => Outcome::acting(Command::Stop),
        // The preset/track distinction is not made here: the active source
        // interprets it (see the doc of `Command::Next`).
        "next" => Outcome::acting(Command::Next),
        "previous" => Outcome::acting(Command::Prev),
        "setvol" => setvol(inst, index, remainder),
        // Deprecated by MPD but still emitted by old clients: relative to
        // the current volume, and clamped here (see `volume`) rather than
        // letting `Command::SetVolume`, which is absolute, overflow.
        "volume" => volume(inst, index, remainder),
        // `seek`/`seekid` ignore their first argument (position or id):
        // `SeekTo` cannot change track at the same time, and MPD only sends
        // this kind of command about what is already playing.
        "seek" => seek(index, "seek", remainder),
        "seekid" => seek(index, "seekid", remainder),
        // The only form that accepts a relative time (`+n`/`-n`), resolved
        // here from `position_s` since `Command::SeekTo` only carries an
        // absolute one.
        "seekcur" => seekcur(inst, index, remainder),
        // **Both names answer exactly the same thing, and it is not a
        // shortcut.** For MPD they are two different origins: `albumart`
        // looks for a file *next to* the track (a `cover.jpg` in its
        // folder), `readpicture` for an image *embedded* in its tags. This
        // device, though, has only **one** cover per track, whatever its
        // origin: the core has already arbitrated between the neighboring
        // file, the embedded tag and the network, and publishes only one.
        // Distinguishing here would ask the plugin for information the
        // display protocol does not carry — and above all, M.A.L.P. tries
        // one then the other: answering only one of the two would make the
        // cover's display depend on the order the client goes about it.
        // Only difference, MPD's own: `readpicture` publishes a `type:`.
        "albumart" => cover(inst, index, "albumart", remainder, binary_limit),
        "readpicture" => cover(inst, index, "readpicture", remainder, binary_limit),
        // The chunk size this client accepts. Handled, not refused: at the
        // version the banner announces (0.23.5), a client takes it for
        // granted and sends it **while connecting** — M.A.L.P. does. An
        // `ACK 5` in the middle of a connection sequence is the worst moment
        // to be refused, and the command moreover has a real effect here: a
        // 500 KiB cover used to take sixty-two round trips in 8 KiB slices.
        "binarylimit" => binarylimit(index, remainder),
        // The volume alone, without the rest of `status` (MPD 0.23). A
        // client that only wants the slider need not re-read fifteen lines.
        "getvol" => Outcome::lines(vec![line("volume", published_volume(inst))]),
        // `load <name>` switches source. It does not append to the queue
        // (MPD *concatenates* a stored playlist there): here the queue **is**
        // the active source's list, so loading it means choosing it.
        // The refusal is no longer fixed: the sources catalog says which
        // names exist.
        "load" => load(inst, index, remainder),
        // **Touching a track in a stored playlist must play it.** It used to
        // be `ACK 5`: the owner reported it, and it is the most ordinary
        // gesture there is once a client can list the sources. A client that
        // "plays" an entry first adds it to the queue (`add`/`addid`), often
        // after emptying it (`clear`).
        //
        // This is **not** a reversal of the refusal of queue editing, which
        // stands whole: reordering, deleting, inserting at a position makes
        // no sense here, the queue *is* the active source's list and it does
        // not belong to us. What these three do is translate "play this
        // entry" into the only vocabulary the device has: choose the source,
        // then the preset. The URI allows it because **we** published it
        // (`currentsong`, `listplaylistinfo`, `lsinfo`) and it names both.
        "add" | "addid" => add(inst, index, cmd, remainder),
        // Accepted without doing anything, and the why must be said: there
        // is no queue to empty — the queue is the source's list. An `ACK`
        // here would interrupt the `clear`/`add`/`play` command list the
        // client sends to play a track, so the refusal would cost exactly
        // the feature we just added. The client will re-read `status` and
        // find the queue unchanged: a benign surprise, against a gesture
        // that works.
        "clear" => Outcome::ok(),
        // **A client's file browser, made useful rather than refused.**
        // `lsinfo` was on the list of deliberate refusals, on the grounds
        // that there is no database to browse. True of files, and false of
        // what the device actually contains: its sources, and each one's
        // presets. The root therefore returns the same stored playlists as
        // `listplaylists` — what real MPD does, publishing them at the root
        // of its music directory — and a source name returns its entries. A
        // client then browses it like a library, the one the device has.
        "lsinfo" => lsinfo(inst, index, remainder),
        // The **database** queries, well-formed and empty.
        //
        // Empty because there is none: nothing indexes tags here, and
        // inventing them would be lying. Well-formed because the refusal,
        // for its part, was a visible defect — a client whose "Albums" tab
        // receives `ACK 5` shows an error, where an empty list shows an
        // empty tab. This is exactly the distinction the `COMMANDS` doc
        // states; it assumed a client that greys out what it does not find
        // in `commands`, and M.A.L.P. does not.
        //
        // `count` is of the same batch but returns two fields rather than
        // nothing: clients read them without testing for them.
        "list" | "listall" | "listallinfo" | "listfiles" => Outcome::ok(),
        "find" | "search" => search(index, cmd, remainder),
        "count" => Outcome::lines(vec![line("songs", 0), line("playtime", 0)]),
        // Everything else gets the same refusal, without distinguishing the
        // unknown from the deliberately unhandled — MPD does not distinguish
        // them either, and `commands` already says what exists. Two of these
        // refusals deserve their reason written down: `update` makes no
        // sense (there is no database to index), and `kill` is refused and
        // not ignored, because powering off the device from the network
        // without authentication would be a capability none of the room's
        // remotes has.
        _ => Outcome::Reject(ack(Ack::Unknown, index, cmd, "unsupported")),
    }
}

/// The word MPD expects for `state`.
fn mpd_state(playback: Playback) -> &'static str {
    match playback {
        Playback::Playing => "play",
        Playback::Paused => "pause",
        Playback::Stopped => "stop",
    }
}

/// Seconds in MPD's decimal format (`12.000`).
fn seconds(s: u32) -> String {
    format!("{:.3}", f64::from(s))
}

/// Where playback stands in the queue: the **dense position** and the
/// **sparse index**, or nothing.
///
/// Nothing at all if the current preset is not in the queue: better a
/// `status` silent on that point than a `song` naming a position the client
/// will not find in the `playlistinfo` it just read. A single place for the
/// two responses that need it (`status` and `currentsong`), otherwise they
/// would end up contradicting each other.
fn current(inst: &Snapshot, queue: &[Entry]) -> Option<(usize, u8)> {
    let preset = inst.state.preset?;
    let position = queue.iter().position(|e| e.index == preset)?;
    Some((position, preset))
}

/// An entry's URI. A scheme of our own: the plugin serves no bytes, and a
/// client only needs a stable key to tell two entries apart.
pub fn uri(source: &str, index: u8) -> String {
    format!("ritornello://{source}/{index}")
}

/// Size of a binary response chunk, in bytes.
///
/// **8 KiB, MPD's own default value** (`binarylimit`), and the figure is not
/// copied out of imitation: it is the cap that a client which sends no
/// `binarylimit` — so every client this plugin can serve, since it does not
/// handle that command — expects never to see exceeded. Serving 64 KiB to a
/// client sized for 8 would be a buffer overrun on its side, caused by us.
///
/// **Set against `MAX_RESPONSE` (1 MiB), the text path's cap.** Both bound
/// the same thing — the bytes one request makes us write — but they have
/// neither the same value nor the same role, and the factor-128 gap is
/// deliberate:
///
/// * `MAX_RESPONSE` must be large because it bounds a **composed** response,
///   whose size is decided by what the client asked for (a list of sixty
///   `playlistinfo`) and not by us. It is a last-resort cap, reached by
///   accumulation.
/// * `MAX_CHUNK` bounds a response whose size **we** choose: the client does
///   not ask for "the whole image", it asks for "starting here", and it is
///   the server that decides how much it gives. Nothing therefore forces us
///   to let a single request write a mebibyte, and a 2 MiB image — a tenth
///   of the cap, see just below — is served in 256 round trips each costing
///   8 KiB of transient buffer instead of a single round trip costing 2048.
///
/// **The round-trip count, and why 8 KiB remains the right choice despite
/// it.** `COVER_MAX_BYTES` is 20 MiB, so the cover cap is served in ~2560
/// round trips, each paying a full network round trip (the client cannot
/// batch them: each request's offset depends on the `size:` the previous one
/// returned, and a command list is sent whole before being read). On a home
/// Wi-Fi with a 20 ms round trip, that is a minute for one image. The figure
/// is true and it is bad; it still does not justify lifting this cap:
///
/// 1. **It describes the cap, not the traffic.** A real cover weighs 75 KiB
///    (Cover Art Archive measurement) to a few hundred kibibytes for an
///    embedded tag: 10 to 50 round trips, a fraction of a second. The
///    20 MiB are the display protocol's refusal bound, not a size the core
///    produces.
/// 2. **8 KiB is not a choice, it is the contract.** It is MPD's default
///    for `binarylimit`, hence what a client that has not raised it expects
///    never to see exceeded — and this plugin does not handle
///    `binarylimit`, so **none** of its clients can have raised it. Serving
///    64 KiB to a client sized for 8 is a buffer overrun on its side,
///    caused by us, in exchange for a few tens of milliseconds.
/// 3. **The lever exists and it is on the right side**: implementing
///    `binarylimit` would let the client ask for larger slices, which is
///    exactly how MPD resolves this trade-off. It is a feature addition,
///    not a fix; what must not be done is raising `MAX_CHUNK` unilaterally.
///
/// Consequence, and it is the point: the binary path **does not go through**
/// the text accumulator and therefore has no amplification factor of its
/// own. The worst *transient* case of a connection doing nothing but
/// `albumart` is `MAX_CHUNK` + the header ≈ 8.3 KiB of buffer, against the
/// ≈ 2.3 MiB the text path allows (see `MAX_RESPONSE`) — three thousandths.
///
/// **What is not bounded per connection, and it must be written as a
/// product** — this is the third time a bound in this file is documented too
/// favorably, so here is the figure and not a nuance. The image lives once
/// in the process **per generation**, not once altogether: `execute` holds
/// its `Snapshot` clone and the binary response holds its own clone of the
/// `Arc`, both for the duration of the `write_all`. A client that requests
/// a chunk then stops reading therefore pins its generation for as long as
/// it likes, and a cover pushed in the meantime creates another one that a
/// second session can pin in turn. The worst case is
/// `MAX_SESSIONS × COVER_MAX_BYTES` = 16 × 20 MiB = **320 MiB**, plus the
/// generation the state holds itself, i.e. **340 MiB** on a one-gibibyte
/// device shared with mpv.
///
/// It takes a deliberate stall *and* covers close to the cap: that is no
/// accident, it is a hostile client — but this port's threat model (open to
/// the whole local network, no password) already accepts that figure, and it
/// is for it that `MAX_SESSIONS` and `MAX_RESPONSE` exist.
///
/// **No mitigation is added here, and that is an argued choice.** The two
/// real levers are out of reach or worse than the ill: lowering
/// `COVER_MAX_BYTES` lives in `ritornello-proto` and concerns the whole
/// device; putting a deadline on the binary `write_all` would introduce the
/// session path's first clock, only to protect against a client that has
/// already chosen to do harm. Serializing binary responses behind a
/// semaphore would be outright harmful: a single stalled client would then
/// deprive all the others of covers. The bound is therefore **known and
/// written down**, which is what was missing.
pub const MAX_CHUNK: usize = 8 * 1024;

/// `albumart <uri> <offset>` and `readpicture <uri> <offset>`: one chunk of
/// the cover of what is playing.
///
/// **The URI is checked strictly against what is playing at this instant**,
/// and that is this arm's design decision. Our `currentsong` publishes
/// `file: ritornello://<source>/<index>`, so `albumart ritornello://radio/17`
/// means "the cover of what preset 17 is playing *right now*" — a URI whose
/// content changes underneath it, which never happens in an ordinary MPD
/// where a URI is a file. Two answers were defensible:
///
/// * **Serve anyway** (ignore the URI). The client always gets an image, but
///   **the wrong one** as soon as its request is one track late, and the
///   damage is lasting: clients cache the cover **under the requested URI**
///   (M.A.L.P. does), so answering station 17's image to a request for
///   station 3 poisons that cache — station 3 will show a wrong image until
///   the client is restarted, and nothing will ever invalidate it.
/// * **Refuse** (chosen). The refusal is transient and repairs itself: the
///   client asks again at the next `player` wakeup, which a cover change
///   precisely triggers (see `apply_cover`). And the strictness costs
///   nothing legitimate — a client asks for the image of what it just read
///   in `currentsong`, that is, the current URI.
///
/// The same requirement applies to the `href`: the held cover must be the
/// one the current state frame announces. Without this second check, the
/// window between the state (sent first) and the cover (sent next) would
/// serve the previous track's image **under the new track's URI** — the
/// poisoning case described above, reached without any client doing anything
/// wrong.
/// Is this command asking for an image the device **has announced** but the
/// plugin does not hold yet?
///
/// **The window it names is the one that made covers disappear.** The core
/// sends the state first, the bytes next (see `display_relay`): at every
/// track change there is therefore an instant — the time to read a
/// `folder.jpg` on a share, or to download it — where the frame already
/// announces the next `cover_href` while the held cover is still the
/// previous one. And that is exactly the instant the client wakes up and
/// asks for the image, since that very frame is what woke it.
///
/// The `albumart` arm then answered "No file exists". The original
/// reasoning — the client will ask again at the next wakeup — holds for an
/// ideal client; M.A.L.P., though, **memorizes the absence** per track so as
/// not to hammer the server, and thus never asked again. The cover stayed
/// blank until the next track, where the same defect started over.
///
/// The answer is to wait, briefly, rather than refuse: the session takes
/// care of it (see `wait_cover`). This function only **says whether there is
/// reason to wait**, and thus stays pure like the rest of the module.
///
/// False as soon as the refusal is final — nothing playing, no image
/// announced, another track's URI, malformed arguments: waiting would change
/// nothing for any of them, and making a client wait three seconds for a
/// certain refusal would be worse than the refusal.
pub fn cover_announced_but_missing(inst: &Snapshot, args: &[String]) -> bool {
    let Some(name) = args.first() else { return false };
    if name != "albumart" && name != "readpicture" {
        return false;
    }
    // Same shape as `cover` requires: two arguments, a numeric offset. A
    // malformed command will be refused, there is nothing to wait for.
    let [_, requested, offset] = args else { return false };
    if offset.parse::<usize>().is_err() {
        return false;
    }
    let Some(announced) = inst.state.track.cover_href.as_deref() else { return false };
    let Some(preset) = inst.state.preset else { return false };
    if *requested != uri(&inst.state.source, preset) {
        return false;
    }
    // The only situation that repairs itself: the announced image is not
    // (yet) the one we hold.
    inst.cover.as_ref().map(|p| p.href.as_str()) != Some(announced)
}

fn cover(
    inst: &Snapshot,
    index: usize,
    name: &str,
    remainder: &[String],
    limit: usize,
) -> Outcome {
    let [requested, offset] = remainder else {
        return Outcome::Reject(ack(Ack::Arg, index, name, "wrong number of arguments"));
    };
    let Ok(offset) = offset.parse::<usize>() else {
        return Outcome::Reject(ack(Ack::Arg, index, name, "integer expected"));
    };
    // The "there is no image here" refusal, common to the four guards that
    // follow: the client need not know *which one* failed, and telling it
    // apart would teach it the plugin's internal state without giving it any
    // different course of action — in all four cases there is no image at
    // this URI, and in all four it will ask again at the next wakeup.
    let missing = || Outcome::Reject(ack(Ack::NoExist, index, name, "No file exists"));
    let Some(cover) = inst.cover.as_ref() else {
        return missing();
    };
    // Nothing numbered is playing: no URI can designate anything, and
    // `currentsong` publishes none anyway.
    let Some(preset) = inst.state.preset else {
        return missing();
    };
    if *requested != uri(&inst.state.source, preset) {
        return missing();
    }
    if Some(cover.href.as_str()) != inst.state.track.cover_href.as_deref() {
        return missing();
    }
    let size = cover.bytes.len();
    // `>` and not `>=`, exactly like MPD: at `offset == size` the client
    // already has everything, and the well-formed answer is an empty chunk —
    // refusing it would fail a client that closes its loop with one request
    // too many. Beyond that, the offset is wrong and it is an argument
    // defect.
    if offset > size {
        return Outcome::Reject(ack(Ack::Arg, index, name, "Offset too large"));
    }
    // The chunk **this client** accepts (see `binarylimit`), never more than
    // the plugin's cap. `MAX_CHUNK` remains the default value, the one a
    // client that asked for nothing receives.
    let end = size.min(offset + limit.min(MAX_CHUNK_CAP));
    // `size:` is the size of the **whole image**, not of the chunk: it is
    // what tells the client how many round trips it has left. Confusing them
    // would make the client stop at the first chunk.
    let mut header = vec![line("size", size)];
    if name == "readpicture" {
        // The only difference between the two commands, and it is MPD's:
        // `readpicture` announces the MIME type, `albumart` does not.
        header.push(line("type", &cover.mime));
    }
    Outcome::Bytes(Binary { header, image: cover.bytes.clone(), chunk: offset..end })
}

/// The volume as the MPD protocol expresses it.
///
/// `muted` overrides the memorized volume. MPD has no mute: clients cut the
/// sound by setting `setvol 0` and therefore expect to read 0 back when it
/// is cut. Reporting 65 on a muted device would display a slider at 65 over
/// silence.
///
/// One place for both `status` and `getvol`: two volumes contradicting each
/// other would be an invisible defect until the day a client reads both.
fn published_volume(inst: &Snapshot) -> u8 {
    if inst.state.muted {
        0
    } else {
        inst.state.volume
    }
}

fn status(inst: &Snapshot) -> Vec<String> {
    let queue = queue(inst);
    let mut lines = vec![line("volume", published_volume(inst))];
    // Reported as zero and **not omitted**: clients always read them, and
    // their absence makes them misbehave. *Writing* them is refused
    // (Task 7), so this is the only place where the plugin publishes a value
    // it cannot change — see the spec, § What the plugin does not do.
    for key in ["repeat", "random", "single", "consume"] {
        lines.push(line(key, 0));
    }
    lines.push(line("playlist", inst.queue_version));
    // The **queue length**, not the maximum of the indices: it is the number
    // of entries a client will ask for. The two coincide on a synthesized
    // queue, and **diverge** as soon as a source enumerates a sparse list —
    // three stations numbered 1, 5 and 99 make `playlistlength: 3`, never
    // 99. Publishing the maximum would make a client ask for ninety-six
    // entries that do not exist.
    lines.push(line("playlistlength", queue.len()));
    // No crossfade here, but the field is read by clients that display a
    // setting. Three decimals like `elapsed` and `duration`.
    lines.push(line("mixrampdb", "0.000"));
    // The **optimistic** state, never the frame's raw one: a client that
    // sends `pause` then `status` in the same stride would otherwise read
    // the state from before its own command, and its button would not have
    // moved.
    lines.push(line("state", mpd_state(inst.playback())));
    if inst.playback() != Playback::Stopped {
        // `song`/`songid` **absent**, not zero: `songid: 0` would designate
        // a real entry, so a client would highlight the wrong line.
        if let Some((position, index)) = current(inst, &queue) {
            lines.push(line("song", position));
            lines.push(line("songid", index));
        }
    }
    if let Some(position_s) = inst.state.position_s {
        // `time` is deprecated but still read; it only appears if the
        // position is known, and an unknown total (a live stream) is written
        // as 0 there — that is what MPD does with streams.
        let total = inst.state.track.duration_s.unwrap_or(0);
        lines.push(line("time", format!("{position_s}:{total}")));
        lines.push(line("elapsed", seconds(position_s)));
    }
    // Independent of the position: Radio France announces a track's duration
    // on a live stream whose progress nobody knows.
    if let Some(duration) = inst.state.track.duration_s {
        lines.push(line("duration", seconds(duration)));
    }
    lines
}

fn currentsong(inst: &Snapshot) -> Vec<String> {
    // Nothing at all — hence a bare `OK` — when no preset is designated.
    // Guarded on `preset` and not on the playback state: a paused playback
    // still has a current track, and MPD publishes it.
    let Some(preset) = inst.state.preset else {
        return Vec::new();
    };
    let queue = queue(inst);
    let mut lines = vec![line("file", uri(&inst.state.source, preset))];
    let track = &inst.state.track;
    // A field absent from `Track` produces **no** line: `Artist: ` is worse
    // than no line, a client displays it as an empty artist. Only the title
    // has a fallback, the preset name — that is the station name, the only
    // thing we know about a stream with no ICY tag.
    if let Some(title) = track.title.as_deref().or(inst.state.preset_name.as_deref()) {
        lines.push(line("Title", title));
    }
    if let Some(artist) = &track.artist {
        lines.push(line("Artist", artist));
    }
    if let Some(album) = &track.album {
        lines.push(line("Album", album));
    }
    // `Date` is the tag's name in the MPD protocol, and it is free-form
    // there: many libraries put a bare year in it. So we put the year as-is,
    // without dressing it up as a full date we do not have.
    if let Some(year) = track.year {
        lines.push(line("Date", year));
    }
    if let Some(duration) = track.duration_s {
        // `Time` as an integer (deprecated), `duration` as a decimal: both,
        // because clients split between the two depending on their age.
        lines.push(line("Time", duration));
        lines.push(line("duration", seconds(duration)));
    }
    if let Some((position, index)) = current(inst, &queue) {
        lines.push(line("Pos", position));
        lines.push(line("Id", index));
    }
    lines
}

/// The lines of one queue entry: its dense position, its sparse index.
fn entry_lines(source: &str, position: usize, entry: &Entry) -> Vec<String> {
    vec![
        line("file", uri(source, entry.index)),
        line("Title", &entry.name),
        line("Pos", position),
        line("Id", entry.index),
    ]
}

/// The lines of a chunk of the queue. `Pos` stays the **absolute** position
/// in the queue and not the rank within the chunk: it is the key the client
/// will use to designate the entry afterward, and shifting it would play
/// something other than what was touched on screen.
fn queue_lines(inst: &Snapshot, file: &[Entry], range: Range<usize>) -> Vec<String> {
    let start = range.start;
    file[range]
        .iter()
        .enumerate()
        .flat_map(|(offset, entry)| entry_lines(&inst.state.source, start + offset, entry))
        .collect()
}

/// Parses an MPD position argument: either a single position (`3`), or a
/// `START:END` range whose **end is exclusive**, `START:` meaning "to the
/// end". Returns the bounds already clamped to the queue, or `None` if the
/// argument is malformed.
///
/// MPD's grammar is `playlistinfo [[SONGPOS] | [START:END]]`, and a client
/// that windows its queue (M.A.L.P. does) asks for `0:100`. Rejecting a
/// well-formed request would make it show an empty queue on the radio's 51
/// stations: the range gets implemented, it does not get declared unhandled.
///
/// **Three out-of-bounds cases that do not answer alike**, and the asymmetry
/// is MPD's own:
/// - a **range** that starts past the end returns an **empty** chunk. A
///   client that windows its queue may ask for `50:100` right after the
///   queue has shrunk; its request is well-formed, the answer is "there is
///   nothing there", not an error.
/// - a **single position** out of bounds stays a rejection: it designates a
///   precise entry that does not exist, and a bare `OK` would suggest a hole
///   in the queue.
/// - `START > END` is **malformed**: no correct client produces it, MPD
///   refuses it too, and accepting it would mask the caller's bug.
fn bounds(arg: &str, length: usize) -> Option<Range<usize>> {
    if let Some((start, end)) = arg.split_once(':') {
        let start: usize = start.parse().ok()?;
        let end = if end.is_empty() { length } else { end.parse::<usize>().ok()? };
        if end < start {
            return None;
        }
        Some(start.min(length)..end.min(length))
    } else {
        let position: usize = arg.parse().ok()?;
        if position >= length {
            return None;
        }
        Some(position..position + 1)
    }
}

fn playlistinfo(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let file = queue(inst);
    let Some(arg) = args.first() else {
        return Outcome::lines(queue_lines(inst, &file, 0..file.len()));
    };
    match bounds(arg, file.len()) {
        Some(range) => Outcome::lines(queue_lines(inst, &file, range)),
        None => Outcome::Reject(ack(Ack::Arg, index, "playlistinfo", "bad song index")),
    }
}

fn plchanges(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let Some(version) = args.first().and_then(|a| a.parse::<u32>().ok()) else {
        return Outcome::Reject(ack(Ack::Arg, index, "plchanges", "integer expected"));
    };
    if version == inst.queue_version {
        // Nothing to say, and that is the whole point of the command: a
        // client holding the current version does not need to receive 51
        // lines. Before parsing the range, then: there is nothing to window
        // in an empty response.
        return Outcome::ok();
    }
    // The whole queue, for lack of knowing what changed inside it: the queue
    // *is* the active source's list of presets, and a source change replaces
    // it wholesale. The grammar is `plchanges VERSION [START:END]`: the same
    // window as `playlistinfo`.
    let file = queue(inst);
    let range = match args.get(1) {
        None => 0..file.len(),
        Some(arg) => match bounds(arg, file.len()) {
            Some(range) => range,
            None => return Outcome::Reject(ack(Ack::Arg, index, "plchanges", "bad song index")),
        },
    };
    Outcome::lines(queue_lines(inst, &file, range))
}

/// `listplaylists`: one entry per source of the sources catalog, **in the
/// order received** — that of `SourceCycle`'s cycling, hence the one the
/// user sees on their remote. Do not sort: the order carries information.
///
/// Nothing at all before the first sources-catalog frame, and that is the
/// truth of that instant: the plugin knows no source yet. A client will
/// re-read after its wakeup on `stored_playlist`.
fn listplaylists(inst: &Snapshot) -> Vec<String> {
    inst.sources_catalog
        .sources
        .iter()
        .flat_map(|s| [line("playlist", &s.name), line("Last-Modified", UNKNOWN_DATE)])
        .collect()
}

/// The lines of a **stored** playlist entry: its URI and its name, and
/// nothing more.
///
/// **No `Pos` or `Id` here**, unlike `entry_lines`: those two labels
/// designate an entry of the *queue*, and a stored playlist is not loaded.
/// Emitting them for a source that is not playing would give a client
/// positions it would never find again in its `playlistinfo` — that is also
/// what MPD does, publishing them only for the queue.
fn playlist_lines(source: &str, entry: &Entry) -> Vec<String> {
    vec![line("file", uri(source, entry.index)), line("Title", &entry.name)]
}

/// The name of a stored playlist as a client wrote it, resolved into a
/// sources-catalog source. `Err` is the already-formatted `ACK 50`.
///
/// A single place for `listplaylistinfo` and `load`: both must answer with
/// the *same* set of names `listplaylists` announces, and letting each look
/// it up on its own would let them drift apart one day.
fn named_playlist<'a>(
    inst: &'a Snapshot,
    index: usize,
    cmd: &str,
    args: &[String],
) -> Result<&'a SourceCatalog, String> {
    let Some(name) = args.first() else {
        return Err(ack(Ack::Arg, index, cmd, "wrong number of arguments"));
    };
    inst.source_catalog(name).ok_or_else(|| {
        // `ACK 50` and not `ACK 2`: the name is well-formed, it is the list
        // that does not exist — the distinction is MPD's own, and a client
        // that reads it knows it should re-read `listplaylists` rather than
        // fix its syntax.
        ack(Ack::NoExist, index, cmd, "no such playlist")
    })
}

/// The entries of a named source, exactly as `listplaylistinfo` and `lsinfo`
/// both render them.
///
/// **The same rule as `queue` wherever it can apply, and it has to be the
/// same one**: a source that cannot enumerate (the cd) carries an empty
/// list, and its entries are synthesized from the count. But `preset_count`
/// only describes the **active** source — for another one, the plugin knows
/// nothing of the count, and an empty list is then the honest answer. The
/// sources catalog carries no count, there is no better answer.
fn source_entries(inst: &Snapshot, source: &SourceCatalog) -> Vec<Entry> {
    if source.presets.is_empty() && source.name == inst.state.source {
        queue(inst)
    } else {
        named_entries(&source.presets)
    }
}

/// `lsinfo [URI]`: the root, or a source's content.
///
/// Without an argument (or on `/`, which clients send for the root), returns
/// the stored playlists — that is, the sources — exactly like
/// `listplaylists`. With a source name, its entries.
///
/// **No `directory:` line**, and that is deliberate: it would make a client
/// expect a tree to descend into, when the device has none. Sources are
/// lists, not folders, and `playlist:` is the right word — the same one
/// under which `load` accepts them.
fn lsinfo(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let target = args.first().map(String::as_str).unwrap_or("");
    if target.is_empty() || target == "/" {
        return Outcome::lines(listplaylists(inst));
    }
    match inst.source_catalog(target) {
        Some(source) => Outcome::lines(
            source_entries(inst, source)
                .iter()
                .flat_map(|e| playlist_lines(&source.name, e))
                .collect(),
        ),
        // `ACK 50` like `listplaylistinfo`: the name is well-formed, it is
        // what it designates that does not exist.
        None => Outcome::Reject(ack(Ack::NoExist, index, "lsinfo", "No such directory")),
    }
}

/// The inverse of [`uri`]: the source and the index one of our URIs
/// designates.
///
/// `None` for everything that is not ours — a file path, an http URL, a
/// truncated URI. Split on the **last** `/`: a source name comes from
/// `plugins.toml` and nothing forbids it from containing one, while the
/// index itself never does.
fn from_uri(uri: &str) -> Option<(&str, u8)> {
    let remainder = uri.strip_prefix("ritornello://")?;
    let (source, index) = remainder.rsplit_once('/')?;
    if source.is_empty() {
        return None;
    }
    Some((source, index.parse().ok()?))
}

/// `add <URI>` / `addid <URI> [POS]`: play the entry this URI designates.
///
/// **`addid`'s position is ignored**, and that is consistent with all the
/// rest: there is no queue to insert into, only a source to choose and a
/// preset to launch. Rejecting it would fail a client that supplies it
/// without caring about it.
///
/// Two commands are emitted when the targeted source is not the active one,
/// a single one otherwise. The order matters and it is guaranteed: the
/// session pushes them in order onto the input channel, which the core pops
/// serially.
fn add(inst: &Snapshot, index: usize, cmd: &str, args: &[String]) -> Outcome {
    let Some(uri) = args.first() else {
        return Outcome::Reject(ack(Ack::Arg, index, cmd, "wrong number of arguments"));
    };
    // `ACK 50`: the URI is well-formed as a string, it is what it designates
    // that does not exist — the distinction MPD makes, and the one that
    // tells the client to re-read rather than fix its syntax.
    let missing = || Outcome::Reject(ack(Ack::NoExist, index, cmd, "No such song"));
    let Some((source, index)) = from_uri(uri) else { return missing() };
    let Some(sources_catalog) = inst.source_catalog(source) else { return missing() };
    // Checked against **this** source's list and not merely against a
    // bound: a sparse table has holes, and an index that falls into one
    // plays nothing. Same rule as `playid`.
    let entries = source_entries(inst, sources_catalog);
    if !index_exists(&entries, index) {
        return missing();
    }
    let mut cmds = Vec::new();
    if source != inst.state.source {
        // The **sources-catalog** name and not the raw argument, like
        // `load`: the two are equal by construction, and emitting the one
        // the core gave us keeps the plugin unable to invent a source name.
        cmds.push(Command::SelectSource(sources_catalog.name.clone()));
    }
    cmds.push(Command::Select(index));
    let mut lines = Vec::new();
    if cmd == "addid" {
        // The only difference between the two commands, and it is MPD's
        // own: `addid` returns the id of what it just added.
        lines.push(line("Id", index));
    }
    Outcome::Reply { lines, cmds }
}

/// `find`/`search`: well-formed, and empty.
///
/// The rejection of missing arguments is kept — it is MPD's own, and a
/// client that sends a truncated request must learn that rather than
/// believe its search returned nothing.
fn search(index: usize, cmd: &str, args: &[String]) -> Outcome {
    if args.is_empty() {
        return Outcome::Reject(ack(Ack::Arg, index, cmd, "too few arguments"));
    }
    Outcome::ok()
}

/// Cap of a binary chunk a client may request, in bytes.
///
/// **64 KiB, and the figure bounds a real expense.** A chunk is a buffer the
/// session writes in one go; the worst case is
/// `MAX_SESSIONS × MAX_CHUNK_CAP`, i.e. 16 × 64 KiB = **1 MiB** on a
/// one-gibibyte device — negligible, where letting a client ask for
/// anything would not be. The gain is clear the other way: a 500 KiB cover
/// goes from sixty-two round trips to eight.
pub const MAX_CHUNK_CAP: usize = 64 * 1024;

/// Floor of a binary chunk. Below it, the textual header would cost more
/// than the bytes it announces.
const MIN_CHUNK: usize = 64;

/// `binarylimit <N>`: the chunk size this client accepts.
///
/// **Bounded on both sides, silently.** MPD refuses a value below its own
/// floor; here the value is clamped into `[MIN_CHUNK, MAX_CHUNK_CAP]` rather
/// than refused, because the upper bound is a decision **of ours** (see
/// `MAX_CHUNK_CAP`) and not a protocol rule: rejecting `binarylimit
/// 1048576` would fail the connection of a perfectly correct client that
/// simply asks for more than we want to serve. A smaller chunk than
/// requested is always legal — the value is a **maximum** the server must
/// not exceed, not a contract of exact size.
fn binarylimit(index: usize, args: &[String]) -> Outcome {
    let Some(n) = args.first().and_then(|a| a.parse::<usize>().ok()) else {
        return Outcome::Reject(ack(Ack::Arg, index, "binarylimit", "integer expected"));
    };
    Outcome::BinaryLimit(n.clamp(MIN_CHUNK, MAX_CHUNK_CAP))
}

fn listplaylistinfo(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let source = match named_playlist(inst, index, "listplaylistinfo", args) {
        Ok(source) => source,
        Err(rejection) => return Outcome::Reject(rejection),
    };
    // Shared with `lsinfo`, which must answer exactly the same thing for the
    // same name: see `source_entries`.
    let entries = source_entries(inst, source);
    Outcome::lines(entries.iter().flat_map(|e| playlist_lines(&source.name, e)).collect())
}

/// `load <name>`: choose the source of that name.
///
/// The plugin itself refuses a name absent from the sources catalog rather
/// than emit a `SelectSource` the core would silently ignore (see the doc of
/// `Command::SelectSource`): it only offers names it has received, so it is
/// up to it to know which ones exist. An `OK` followed by nothing would be
/// the worst possible answer for a client, which would wait for a queue
/// change that never arrives.
fn load(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    match named_playlist(inst, index, "load", args) {
        // The **sources-catalog** name and not the raw argument: the two
        // are equal by construction (`source_catalog` compares exactly),
        // but emitting the one the core gave us keeps the plugin unable to
        // invent a source name.
        Ok(source) => Outcome::acting(Command::SelectSource(source.name.clone())),
        Err(rejection) => Outcome::Reject(rejection),
    }
}

fn stats(inst: &Snapshot) -> Vec<String> {
    // `uptime` at 0 **deliberately**: getting it right would require
    // remembering a start instant, hence one more clock in a module that has
    // none, for a value no client here uses. Same reason for the cumulative
    // playback durations.
    vec![
        line("artists", 0),
        line("albums", 0),
        line("songs", queue(inst).len()),
        line("uptime", 0),
        line("db_playtime", 0),
        line("db_update", 0),
        line("playtime", 0),
    ]
}

/// What a subsystem name written in an `idle` amounts to.
enum IdleName {
    /// One of the four this plugin can make move.
    Ours(Subsystem),
    /// An MPD-vocabulary subsystem we will **never** emit.
    NeverEmitted,
    /// A word MPD itself does not know.
    Unknown,
}

/// The MPD name of a subsystem, as a client writes it into its `idle`.
///
/// **The whole of MPD's vocabulary, not just ours.** That is the distinction
/// that decides whether a client starts up at all: everything built on
/// libmpdclient's `mpd_send_idle_mask` sends an explicit list — in practice
/// `database update stored_playlist playlist player mixer output options` —
/// and an `ACK` on its first `idle` makes it loop or give up. Rejecting a
/// word MPD does not know is fair; rejecting a **legal** word is the same
/// defect seen from the other side.
///
/// A legal subsystem we never emit is therefore accepted then silently
/// dropped, and the resulting wait may never end. That is the correct MPD
/// behavior and not an oversight: the client asked to be told if it
/// changed, and it never does.
fn idle_name(name: &str) -> IdleName {
    match name {
        "player" => IdleName::Ours(Subsystem::Player),
        "mixer" => IdleName::Ours(Subsystem::Mixer),
        "playlist" => IdleName::Ours(Subsystem::Playlist),
        "stored_playlist" => IdleName::Ours(Subsystem::StoredPlaylist),
        // The rest of MPD's vocabulary. None of these has a trigger here:
        // there is no database to index (`database`, `update`), a single
        // output we do not steer (`output`), no modifiable option
        // (`options`), no partition, no attached sticker, no subscription,
        // no message, no neighbor, no mount announced on this protocol.
        "database" | "update" | "output" | "options" | "partition" | "sticker"
        | "subscription" | "message" | "neighbor" | "mount" => IdleName::NeverEmitted,
        _ => IdleName::Unknown,
    }
}

fn idle(index: usize, args: &[String]) -> Outcome {
    if args.is_empty() {
        // Without an argument, all subsystems count.
        return Outcome::Wait(vec![
            Subsystem::Player,
            Subsystem::Mixer,
            Subsystem::Playlist,
            Subsystem::StoredPlaylist,
        ]);
    }
    let mut subsystems = Vec::new();
    for name in args {
        match idle_name(name) {
            // De-duplicated, like `mark` on the state side: `idle player
            // player` describes only a single wait.
            IdleName::Ours(s) => {
                if !subsystems.contains(&s) {
                    subsystems.push(s);
                }
            }
            // Accepted then dropped: see `idle_name`. The list may end up
            // empty, and that is a wait that will never end — the correct
            // answer, not an oversight.
            IdleName::NeverEmitted => {}
            // A word MPD does not know: refused and not ignored, otherwise
            // a client that misspelled its subsystem would stay silent
            // forever, which is far harder to diagnose than an `ACK`.
            IdleName::Unknown => {
                return Outcome::Reject(ack(Ack::Arg, index, "idle", "unrecognized idle event"))
            }
        }
    }
    Outcome::Wait(subsystems)
}

// ----------------------------------------------------------------------
// The action commands: what asks the device to do something.
// ----------------------------------------------------------------------

/// Translates an MPD position (the **rank**, 0-based, the one `Pos`
/// publishes) into the preset index found there. `None` if the position is
/// past the queue.
///
/// Extracted as a pure function, separate from `play`, so it can also be
/// tested against a hand-built queue. It is the only allowed path from
/// position to index: as soon as a source enumerates a sparse list, "the
/// index minus one" is no longer the rank, and the shift a subtraction
/// would introduce would play a station neighboring the one touched on
/// screen.
fn position_to_index(file: &[Entry], position: usize) -> Option<u8> {
    file.get(position).map(|e| e.index)
}

/// True if this preset index really exists in the queue — not merely
/// within the bounds of its maximum.
///
/// The distinction has no effect on a synthesized queue ("existing" and
/// "being ≤ the maximum" are the same thing there) and is decisive on a
/// sparse queue, where `preset_count` stays a maximum and not a count: a
/// `playid` on a hole in the sequence must be rejected, where a bound
/// comparison would wrongly let it through.
fn index_exists(file: &[Entry], index: u8) -> bool {
    file.iter().any(|e| e.index == index)
}

/// An absolute MPD time, in truncated seconds. `None` if non-numeric,
/// non-finite or negative — never a negative time silently clamped to zero
/// for this form (unlike `seekcur`'s relative resolution, where zero is the
/// right answer to too large a rewind).
///
/// **`inf` and `nan` are non-numeric for this protocol**, even though
/// `f64::from_str` accepts them: `seek 0 inf` used to return
/// `SeekTo(u32::MAX)` and `seek 0 nan` used to return `SeekTo(0)`, both
/// **silently**, against the rule this module states twelve lines up: an
/// absent or non-numeric argument is an `Ack::Arg`, never a silent defect.
/// The same class as `volume`'s `i16` overflow, on the same
/// authentication-free port, two meters away.
fn absolute_time(s: &str) -> Option<u32> {
    let v: f64 = s.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        None
    } else {
        Some(v as u32)
    }
}

fn play(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let Some(arg) = args.first() else {
        // The Play key, not a selection: restart whatever was loaded (or
        // start, for a source that knows what to do even while stopped —
        // that is for it to decide, not for this plugin).
        return Outcome::acting(Command::PlayPause);
    };
    let Ok(position) = arg.parse::<usize>() else {
        return Outcome::Reject(ack(Ack::Arg, index, "play", "need a positive integer"));
    };
    match position_to_index(&queue(inst), position) {
        Some(index) => Outcome::acting(Command::Select(index)),
        None => Outcome::Reject(ack(Ack::Arg, index, "play", "bad song index")),
    }
}

fn playid(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let Some(id) = args.first().and_then(|a| a.parse::<u8>().ok()) else {
        return Outcome::Reject(ack(Ack::Arg, index, "playid", "need a positive integer"));
    };
    if index_exists(&queue(inst), id) {
        Outcome::acting(Command::Select(id))
    } else {
        Outcome::Reject(ack(Ack::Arg, index, "playid", "no such song"))
    }
}

/// `pause [0|1]`. Without an argument, toggles; with one, only emits if the
/// state differs from the target — that is what closes the race described
/// in the spec (§ `pause` in `PlayerState.playback`): a `pause 1` resent
/// twice by a client that did not see the confirmation must not relaunch
/// playback.
///
/// **While stopped, never emits anything**, whatever the argument:
/// `PlayPause` would start a playback that neither the source nor this
/// plugin know what or where of (see `SharedState::acknowledge_optimistic`),
/// which is not what a client asked for by pressing "pause". Argument
/// validation happens **before** this guard: a malformed `pause 2` must
/// still be an `ACK` even while stopped, not silently swallowed.
fn pause(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let target = match args.first().map(String::as_str) {
        None => None,
        Some("0") => Some(Playback::Playing),
        Some("1") => Some(Playback::Paused),
        Some(_) => return Outcome::Reject(ack(Ack::Arg, index, "pause", "boolean expected")),
    };
    if inst.playback() == Playback::Stopped {
        return Outcome::ok();
    }
    match target {
        None => Outcome::acting(Command::PlayPause),
        Some(target) if inst.playback() != target => Outcome::acting(Command::PlayPause),
        Some(_) => Outcome::ok(),
    }
}

/// `setvol <0-100>`: sets the volume, and **lifts the mute if it is above
/// zero**.
///
/// The first point is the protocol; the second is the only way a client
/// MPD has to turn this device's sound back on.
///
/// **Why it had to be done.** MPD has no mute, so `status` publishes
/// `volume: 0` as soon as the device is muted (see `status`) — fair enough,
/// clients cut the sound by setting `setvol 0` and expect to read back 0.
/// But *no* MPD command could lift that mute: a client would raise its
/// slider, `SetVolume(40)` would go out, the volume would really change,
/// and the sound stayed cut. The user had no way to get sound from their
/// phone anymore — only the room's remote could fix it. A user raising a
/// slider unambiguously asks to hear something; that is the playback we
/// grant.
///
/// **Emitted conditionally, because `Command::Mute` is a toggle** and not a
/// set: emitting it while nothing is cut would *cut* the sound. The guard
/// on `state.muted` is therefore the same conditional shape `pause 0`/`pause
/// 1` uses against `playback`, and for the same reason.
///
/// **The order of the two commands does not change the result**, and this
/// has to be written down because this paragraph first claimed the
/// opposite: it asserted that the core, while lifting the mute, would reset
/// the memorized volume, hence that the volume had to be set afterward.
/// That is false, and a false reason is worse than no reason — it makes a
/// restoration mechanism seem to exist, from which a player would deduce
/// things. The core's `Command::Mute` arm does `muted = !muted` then
/// `set_mute(muted)`, and nothing else: the level and the mute are two
/// independent properties, two separate calls to mpv. `SetVolume(40)` then
/// `Mute`, or `Mute` then `SetVolume(40)`, both leave a device unmuted at
/// 40.
///
/// The order chosen — `SetVolume` first — only plays out on the
/// **interval** between the two, which does genuinely exist: they cross the
/// input channel one after the other, and each waits on mpv.
///
/// * **What is heard, and it is the reason that weighs.** Setting the level
///   while the output is still muted is inaudible, so the sound already
///   comes back *at* 40. The reverse order would bring it back to the
///   memorized level — up to 100 — for one round trip before dropping. On a
///   device whose memorized volume can be well above what the client asks
///   for, this is the only one of the two differences that is noticeable.
/// * **What is seen.** Both commands call `show_overlay`, which reads the
///   instant's `muted` and `volume`. The *final* overlay says "40%" in
///   either order; only the intermediate one differs, and the order chosen
///   here shows "muted" — a word still true at that instant — instead of
///   the old level, a number that no longer is.
///
/// **`setvol 0` does not cut the sound for all that**, and that is the
/// unchanged reverse rule: see the spec, § "Mute, a case not to miss".
/// `SetVolume(0)` sets zero, `Mute` toggles; confusing them would make a
/// client raising the volume after a `setvol 0` find the sound still cut —
/// exactly the defect fixed here, reintroduced from the other end.
fn setvol(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    match args.first().and_then(|a| a.parse::<u8>().ok()) {
        Some(v) if v <= 100 => Outcome::Reply { lines: Vec::new(), cmds: unmute(inst, v) },
        _ => Outcome::Reject(ack(Ack::Arg, index, "setvol", "invalid volume")),
    }
}

/// The commands for setting the volume: `SetVolume`, plus `Mute` if the
/// device is muted and the requested volume is not zero.
///
/// A single place for `setvol` and `volume`: both are the same gesture
/// ("turn the sound up"), and letting one unmute without the other would
/// make the sound's return depend on the client's age — `volume` is
/// deprecated by MPD, so it is the old half of the fleet that would stay
/// stuck.
fn unmute(inst: &Snapshot, level: u8) -> Vec<Command> {
    let mut cmds = vec![Command::SetVolume(level)];
    if level > 0 && inst.state.muted {
        cmds.push(Command::Mute);
    }
    cmds
}

/// `volume <±n>`: deprecated by MPD but still emitted by old clients.
/// Relative to the current volume and **clamped here** — `Command::SetVolume`
/// is absolute, so this module is the one that must compute and clamp it,
/// not the core.
fn volume(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    match args.first().and_then(|a| a.parse::<i16>().ok()) {
        Some(delta) => {
            // Widened to `i32` before the addition: `delta` covers the
            // whole of `i16` (±32767), and even a low current volume (1)
            // added to `i16::MAX` overflows `i16` before `.clamp` can act —
            // a panic in debug/test (overflow checks are on by default), a
            // wrong value in release. `i32` holds both operands (volume ≤
            // 100, delta ≤ 32767) with no risk of overflow, so the clamp
            // stays the only place that bounds anything.
            let new_volume = (i32::from(inst.state.volume) + i32::from(delta)).clamp(0, 100) as u8;
            // The same mute-lifting as `setvol`, and through the same path:
            // see `unmute`. The computation starts from the **memorized**
            // volume and not from the zero `status` publishes when it is
            // cut — it is the only starting point that makes sense, and it
            // makes `volume +5` on a muted device equivalent to what the
            // remote would do.
            Outcome::Reply { lines: Vec::new(), cmds: unmute(inst, new_volume) }
        }
        None => Outcome::Reject(ack(Ack::Arg, index, "volume", "invalid volume")),
    }
}

/// `seek <POS> <T>` / `seekid <ID> <T>`: the first argument (position or id)
/// is ignored — `Command::SeekTo` cannot change track at the same time, and
/// MPD only sends this kind of command about what is already playing. `T`
/// is always absolute here; only `seekcur` accepts the relative form (see
/// `seekcur`).
fn seek(index: usize, cmd: &str, args: &[String]) -> Outcome {
    match args.get(1).and_then(|a| absolute_time(a)) {
        Some(t) => Outcome::acting(Command::SeekTo(t)),
        None => Outcome::Reject(ack(Ack::Arg, index, cmd, "float expected")),
    }
}

/// `seekcur <T>`: `T` is `+n`, `-n`, or a decimal absolute. `Command` only
/// carries an absolute positioning, so the relative form is resolved here,
/// from `position_s`, truncated to seconds and **never negative** — a
/// rewind larger than the position returns `0`, not a negative time.
fn seekcur(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let reject = |message: &str| Outcome::Reject(ack(Ack::Arg, index, "seekcur", message));
    let Some(arg) = args.first() else {
        return reject("float expected");
    };
    let seconds = if arg.starts_with('+') || arg.starts_with('-') {
        let Ok(delta) = arg.parse::<f64>() else {
            return reject("float expected");
        };
        // The same rule as `absolute_time`, on the other form: `+inf` and
        // `-nan` parse, and without this guard `seekcur +inf` used to
        // return `SeekTo(u32::MAX)` silently. The relative form tolerates
        // the negative (too large a rewind is worth zero), never the
        // non-finite — there is no position "infinity" resolves to.
        if !delta.is_finite() {
            return reject("float expected");
        }
        let Some(base) = inst.state.position_s else {
            // Nothing to resolve from: a relative time with no known
            // starting point would invent a time, which no silent default
            // must do (see the brief's rule on out-of-bounds arguments).
            return reject("no current position");
        };
        // `.max(0.0)` is explicit rather than implicit: the `f64 -> u32`
        // conversion already saturates to 0 on a negative float since Rust
        // 1.45, so removing it would not change this particular result —
        // but nothing should depend on knowing that by eye to read this
        // line.
        (f64::from(base) + delta).max(0.0) as u32
    } else {
        match absolute_time(arg) {
            Some(t) => t,
            None => return reject("float expected"),
        }
    };
    Outcome::acting(Command::SeekTo(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SharedState;
    use ritornello_proto::{SourcesCatalog, Track, PlayerState};

    // ------------------------------------------------------------------
    // The reference snapshots.
    //
    // One constructor per situation, all built on `snapshot_from`: **Tasks 7
    // and 13** reuse these same constructors (`snapshot_paused`,
    // `snapshot_at_volume`, `snapshot_with_presets`…), so they live in a
    // single place and grow by addition, never by tweaking existing ones —
    // a Task 6 test that changed a reference value because Task 7 needed a
    // field would be a false failure.
    // ------------------------------------------------------------------

    /// Wraps a frame into a coherent snapshot.
    ///
    /// `optimistic_playback` copies `state.playback`: that is the state at
    /// rest, once the confirming frame has arrived. A test that wants to
    /// see them diverge instead sets it itself — that is exactly the
    /// property it verifies.
    ///
    /// `queue_version` is 7 and not 0, so that `playlist: 7` cannot pass by
    /// accident behind an implementation that would publish a constant.
    fn snapshot_from(state: PlayerState) -> Snapshot {
        Snapshot { optimistic_playback: state.playback, state, queue_version: 7, ..Default::default() }
    }

    /// The radio stopped: three presets, nothing playing.
    fn radio_stopped() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 40,
            preset_count: Some(3),
            ..Default::default()
        }
    }

    /// The radio on its second preset, with a full track.
    fn radio_playing(playback: Playback) -> PlayerState {
        PlayerState {
            playback,
            preset: Some(2),
            preset_name: Some("France Inter".into()),
            position_s: Some(12),
            track: Track {
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                album: Some("Kind of Blue".into()),
                duration_s: Some(545),
                year: Some(1959),
                // The MPD protocol has no link field: the plugin reads
                // none, same reason as `cover_href` below.
                links: Vec::new(),
                origin: Some("musicbrainz".into()),
                // The MPD protocol has no cover field: the plugin reads
                // none, but the literal must stay complete — that is what
                // forces this test to be revisited when a field appears.
                cover_href: None,
                cover_origin: None,
                // Same reason: the MPD protocol carries no provenance, the
                // plugin reads none, and the literal stays complete so
                // that an added field forces a pass back through here.
                provenance: Default::default(),
            },
            ..radio_stopped()
        }
    }

    fn snapshot_stopped() -> Snapshot {
        snapshot_from(radio_stopped())
    }

    /// The sound cut, but a very real memorized volume.
    fn snapshot_muted(volume: u8) -> Snapshot {
        snapshot_from(PlayerState { volume, muted: true, ..radio_stopped() })
    }

    fn snapshot_playing() -> Snapshot {
        snapshot_from(radio_playing(Playback::Playing))
    }

    fn snapshot_paused() -> Snapshot {
        snapshot_from(radio_playing(Playback::Paused))
    }

    /// A station playing with not the slightest ICY tag: it has a preset
    /// name, and nothing else.
    fn snapshot_without_title() -> Snapshot {
        snapshot_from(PlayerState {
            playback: Playback::Playing,
            preset: Some(1),
            preset_name: Some("Chérie FM".into()),
            ..radio_stopped()
        })
    }

    /// A source that declares a preset count without naming them.
    fn snapshot_without_presets(source: &str, count: u8) -> Snapshot {
        snapshot_from(PlayerState {
            source: source.into(),
            preset_count: Some(count),
            ..Default::default()
        })
    }

    /// A sources-catalog entry, as the core emits one per declared source.
    /// An empty preset list is the cd's truth, which stays the default body
    /// of `list_presets`.
    fn make_source_catalog(name: &str, presets: &[(u8, &str)]) -> SourceCatalog {
        SourceCatalog {
            name: name.to_string(),
            presets: presets
                .iter()
                .map(|(index, name)| Preset { index: *index, name: (*name).to_string() })
                .collect(),
        }
    }

    /// The snapshot of a device whose core has published its sources
    /// catalog, the `active` source being the one the last state frame
    /// designates.
    ///
    /// **Two details of realism, because a snapshot no producer could ever
    /// emit proves nothing**:
    /// - the active source is **added to the sources catalog** if it is not
    ///   already there, with an empty list: the core's sources catalog
    ///   names *every* declared source, and the cd is present in it without
    ///   knowing how to enumerate. A sources catalog that ignored the
    ///   playing source does not exist.
    /// - `preset_count` is the **maximum** of the active source's indices,
    ///   and not their count: that is what `Stations::preset_count` really
    ///   returns (`radio/src/config.rs`). Three stations 1, 5 and 99
    ///   therefore make `preset_count: Some(99)` — the exact shape that
    ///   traps an implementation confusing count and maximum. `None` when
    ///   the active source enumerates nothing, like a source that declared
    ///   nothing.
    fn snapshot_catalog(active: &str, sources: &[(&str, &[(u8, &str)])]) -> Snapshot {
        let mut sources_catalog =
            SourcesCatalog { sources: sources.iter().map(|(n, p)| make_source_catalog(n, p)).collect() };
        if !sources_catalog.sources.iter().any(|s| s.name == active) {
            sources_catalog.sources.push(make_source_catalog(active, &[]));
        }
        let maximum = sources_catalog
            .sources
            .iter()
            .find(|s| s.name == active)
            .and_then(|s| s.presets.iter().map(|p| p.index).max());
        Snapshot {
            sources_catalog,
            ..snapshot_from(PlayerState { source: active.into(), preset_count: maximum, ..Default::default() })
        }
    }

    /// A sources catalog of named sources without presets, the first one
    /// being active.
    ///
    /// This is the shape the sources catalog has **at startup**: the core
    /// knows its sources from wiring onward and fills in their presets as
    /// the `ListPresets` responses arrive over the update channel.
    fn snapshot_with_catalog(names: &[&str]) -> Snapshot {
        let sources: Vec<(&str, &[(u8, &str)])> = names.iter().map(|n| (*n, &[][..])).collect();
        snapshot_catalog(names.first().copied().unwrap_or_default(), &sources)
    }

    /// A source whose presets are named, and which is playing.
    ///
    /// The indices and names are **kept exactly as given**, sparse
    /// included: it is the sources catalog that carries them, and `queue`
    /// copies them without deriving a rank from an index.
    fn snapshot_with_presets(source: &str, presets: &[(u8, &str)]) -> Snapshot {
        snapshot_catalog(source, &[(source, presets)])
    }

    /// A source playing while another is in the sources catalog: the case
    /// that motivated the workaround for the core-side guard
    /// (`handle_source_update` gives back control on a frame that does not
    /// come from the active source, since the sources catalog describes
    /// every source).
    fn snapshot_active_on(active: &str, sources: &[(&str, &[(u8, &str)])]) -> Snapshot {
        snapshot_catalog(active, sources)
    }

    /// A given volume, with nothing else around it.
    fn snapshot_at_volume(volume: u8) -> Snapshot {
        snapshot_from(PlayerState { volume, ..radio_stopped() })
    }

    /// A known position in what is playing, with nothing else around it.
    fn snapshot_at_position(position_s: u32) -> Snapshot {
        snapshot_from(PlayerState { position_s: Some(position_s), ..radio_stopped() })
    }

    fn handle_words(inst: &Snapshot, index: usize, words: &[&str]) -> Outcome {
        let args: Vec<String> = words.iter().map(|m| (*m).to_string()).collect();
        handle(inst, index, &args, MAX_CHUNK)
    }

    /// The lines of a response, or a panic naming what was received
    /// instead — an unexpected `Reject` must be readable in the failure
    /// message.
    fn handle_ok(inst: &Snapshot, words: &[&str]) -> Vec<String> {
        match handle_words(inst, 0, words) {
            Outcome::Reply { lines, .. } => lines,
            other => panic!("expected Reply for {words:?}, got {other:?}"),
        }
    }

    /// The commands emitted by a response, or a panic naming what was
    /// received instead — `handle_ok`'s counterpart for Task 7's tests.
    fn cmds(inst: &Snapshot, words: &[&str]) -> Vec<Command> {
        match handle_words(inst, 0, words) {
            Outcome::Reply { cmds, .. } => cmds,
            other => panic!("expected Reply for {words:?}, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // The queue
    // ------------------------------------------------------------------

    #[test]
    fn without_a_list_the_queue_is_synthesized_from_the_count() {
        // The cd: three tracks, no name. The sequence is dense, `Pos = Id -
        // 1`, and it starts at 1 — the index `Command::Select` expects, not
        // a 0-based rank.
        let inst = snapshot_without_presets("cd", 3);
        assert_eq!(
            queue(&inst),
            vec![
                Entry { index: 1, name: "1".into() },
                Entry { index: 2, name: "2".into() },
                Entry { index: 3, name: "3".into() },
            ]
        );
    }

    #[test]
    fn nothing_declared_gives_an_empty_queue_and_not_the_historical_grid() {
        // `preset_count: None` means "the source declared nothing", which
        // the UI translates as its 1-9 grid. Here that would be wrong:
        // announcing nine entries would make a client ask for nine things
        // none of which exists.
        let inst = snapshot_from(PlayerState { source: "aux".into(), ..Default::default() });
        assert!(queue(&inst).is_empty());
        assert!(handle_ok(&inst, &["status"]).contains(&"playlistlength: 0".to_string()));
        assert!(handle_ok(&inst, &["playlistinfo"]).is_empty());
    }

    #[test]
    fn a_real_list_takes_precedence_over_the_synthesis() {
        // The branch Task 13 puts **first**: as soon as the sources catalog
        // names the active source's presets, they are the queue — with
        // their indices exactly as given, sparse included, and their real
        // names. An implementation stuck on the synthesis would return
        // 1..=99.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(
            queue(&inst),
            vec![
                Entry { index: 1, name: "FIP".into() },
                Entry { index: 5, name: "Nova".into() },
                Entry { index: 99, name: "TSF".into() },
            ]
        );
    }

    #[test]
    fn an_active_source_that_cannot_enumerate_falls_back_to_the_synthesis() {
        // The cd is indeed in the sources catalog, with an **empty** list:
        // that means "I only have numbers", not "I have nothing". Without
        // this fallback on `preset_count`, a inserted disc's twelve tracks
        // would disappear the day the sources catalog arrives — a
        // regression only this combination (sources catalog present, empty
        // list) can show.
        let inst = Snapshot {
            sources_catalog: SourcesCatalog { sources: vec![make_source_catalog("cd", &[])] },
            ..snapshot_without_presets("cd", 12)
        };
        assert_eq!(queue(&inst).len(), 12);
        assert_eq!(queue(&inst)[11], Entry { index: 12, name: "12".into() });
    }

    #[test]
    fn the_queue_follows_the_active_source_and_not_the_first_of_the_catalog() {
        // The sources catalog describes every source; the queue is only
        // made of the one that is playing. Taking the sources catalog's
        // first entry would publish the radio's stations while a disc is
        // spinning.
        let inst = snapshot_active_on("cd", &[("radio", &[(1, "FIP"), (5, "Nova")]), ("cd", &[])]);
        assert!(queue(&inst).is_empty(), "the cd cannot enumerate and declared nothing");
    }

    #[test]
    fn positions_are_dense_where_indices_are_sparse() {
        // THE project's test, end to end through `handle`: on stations 1, 5
        // and 99, the published positions are 0, 1, 2 — and the `Id`s stay
        // 1, 5, 99. Any rank derived by subtraction (`Pos = Id - 1`) would
        // publish 0, 4, 98 here.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(
            handle_ok(&inst, &["playlistinfo"]),
            vec![
                "file: ritornello://radio/1",
                "Title: FIP",
                "Pos: 0",
                "Id: 1",
                "file: ritornello://radio/5",
                "Title: Nova",
                "Pos: 1",
                "Id: 5",
                "file: ritornello://radio/99",
                "Title: TSF",
                "Pos: 2",
                "Id: 99",
            ]
        );
    }

    #[test]
    fn playlistlength_is_the_list_length_not_the_maximum_index() {
        // The property nothing pinched before a sparse queue existed: three
        // stations numbered 1, 5 and 99 make `playlistlength: 3`. An
        // implementation that published `preset_count` (99, the
        // **maximum**) would make a client ask for ninety-six entries that
        // do not exist. The fixture confirms it: `preset_count` is indeed
        // 99 here, so the two values are clearly distinct.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(inst.state.preset_count, Some(99), "the fixture must indeed carry the maximum");
        let lines = handle_ok(&inst, &["status"]);
        assert!(lines.contains(&"playlistlength: 3".to_string()), "{lines:?}");
        assert!(!lines.contains(&"playlistlength: 99".to_string()), "{lines:?}");
    }

    #[test]
    fn stats_counts_the_entries_and_not_the_maximum_index() {
        // The twin of the previous test on `stats`: same possible
        // confusion, same silence of the tests before a sparse queue
        // existed. `songs` is a count of entries.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        let lines = handle_ok(&inst, &["stats"]);
        assert!(lines.contains(&"songs: 3".to_string()), "{lines:?}");
        assert!(!lines.contains(&"songs: 99".to_string()), "{lines:?}");
    }

    #[test]
    fn play_on_a_sparse_list_selects_the_index_of_the_requested_rank() {
        // `position_to_index` seen from `handle`, with a sparse queue the
        // producer can really emit: `play 1` must select 5.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(cmds(&inst, &["play", "0"]), vec![Command::Select(1)]);
        assert_eq!(cmds(&inst, &["play", "1"]), vec![Command::Select(5)]);
        assert_eq!(cmds(&inst, &["play", "2"]), vec![Command::Select(99)]);
        assert!(
            matches!(handle_words(&inst, 0, &["play", "3"]), Outcome::Reject(_)),
            "three entries, so rank 3 does not exist — even though index 3 is under the maximum"
        );
    }

    #[test]
    fn playid_on_a_hole_of_the_sparse_list_is_refused() {
        // `index_exists` seen from `handle`: 2 is under the maximum (99)
        // but is not a station. A bound comparison would let it through,
        // and the core would silently ignore the `Select`.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(cmds(&inst, &["playid", "99"]), vec![Command::Select(99)]);
        assert!(matches!(handle_words(&inst, 0, &["playid", "2"]), Outcome::Reject(_)));
    }

    #[test]
    fn the_current_track_of_a_sparse_list_publishes_the_rank_and_the_index() {
        // `status` and `currentsong` must agree on the two numbers:
        // `song`/`Pos` is the rank (1 for the second entry), `songid`/`Id`
        // the index (5). Confusing them would highlight the wrong line.
        // The frame carries `preset: Some(5)` and the name that goes with
        // it, as the core publishes them together; the sources catalog
        // carries the three stations.
        let base = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        let inst = Snapshot {
            state: PlayerState {
                playback: Playback::Playing,
                preset: Some(5),
                preset_name: Some("Nova".into()),
                ..base.state
            },
            optimistic_playback: Playback::Playing,
            ..base
        };
        let status = handle_ok(&inst, &["status"]);
        assert!(status.contains(&"song: 1".to_string()), "{status:?}");
        assert!(status.contains(&"songid: 5".to_string()), "{status:?}");
        let current = handle_ok(&inst, &["currentsong"]);
        assert!(current.contains(&"Pos: 1".to_string()), "{current:?}");
        assert!(current.contains(&"Id: 5".to_string()), "{current:?}");
        assert!(current.contains(&"Title: Nova".to_string()), "{current:?}");
    }

    // ------------------------------------------------------------------
    // `status`
    // ------------------------------------------------------------------

    #[test]
    fn status_publishes_its_fields_in_the_expected_order() {
        // Order and presence are the contract: a client reads these lines
        // in the order MPD emits them, and a missing field makes some give
        // up. Equality of the whole vector, then, not `contains`.
        assert_eq!(
            handle_ok(&snapshot_playing(), &["status"]),
            vec![
                "volume: 40",
                "repeat: 0",
                "random: 0",
                "single: 0",
                "consume: 0",
                "playlist: 7",
                "playlistlength: 3",
                "mixrampdb: 0.000",
                "state: play",
                "song: 1",
                "songid: 2",
                "time: 12:545",
                "elapsed: 12.000",
                "duration: 545.000",
            ]
        );
    }

    #[test]
    fn status_returns_zero_volume_when_the_sound_is_cut() {
        // MPD has no mute: clients cut the sound by setting `setvol 0`, so
        // they expect to read 0 back when it is cut.
        let inst = snapshot_muted(65);
        assert!(handle_ok(&inst, &["status"]).contains(&"volume: 0".to_string()));
    }

    #[test]
    fn status_names_no_song_while_stopped() {
        // `songid: 0` would designate a real entry: the field must be
        // absent.
        let lines = handle_ok(&snapshot_stopped(), &["status"]);
        assert!(lines.contains(&"state: stop".to_string()));
        assert!(!lines.iter().any(|l| l.starts_with("song")), "{lines:?}");
    }

    #[test]
    fn status_names_no_song_even_stopped_on_a_preset() {
        // The guard on playback state, distinct from the one on `preset`:
        // `snapshot_stopped` only proves the second one (it has no preset
        // at all). A stopped source that kept its own must not designate
        // any song in `status`.
        let mut inst = snapshot_stopped();
        inst.state.preset = Some(2);
        let lines = handle_ok(&inst, &["status"]);
        assert!(!lines.iter().any(|l| l.starts_with("song")), "{lines:?}");
        // `currentsong`, for its part, keeps its track: the asymmetry is
        // MPD's own, which publishes a current track even while stopped.
        // The two guards are therefore genuinely distinct, and this test
        // says which is which.
        assert!(handle_ok(&inst, &["currentsong"])
            .contains(&"file: ritornello://radio/2".to_string()));
    }

    #[test]
    fn status_reports_the_three_states() {
        for (inst, expected) in [
            (snapshot_playing(), "state: play"),
            (snapshot_paused(), "state: pause"),
            (snapshot_stopped(), "state: stop"),
        ] {
            let lines = handle_ok(&inst, &["status"]);
            assert!(lines.contains(&expected.to_string()), "{expected} missing from {lines:?}");
            // A single `state`, and it is the right one: an implementation
            // that emitted all three would pass the `contains` above.
            assert_eq!(lines.iter().filter(|l| l.starts_with("state: ")).count(), 1);
        }
    }

    #[test]
    fn status_publishes_the_optimistic_state_and_not_the_frames() {
        // `pause`'s race: a client that sends `pause` then `status` in the
        // same stride must read the effect of its own command, even if the
        // confirming frame has not arrived yet.
        let mut inst = snapshot_paused();
        inst.optimistic_playback = Playback::Playing;
        assert!(handle_ok(&inst, &["status"]).contains(&"state: play".to_string()));
    }

    #[test]
    fn the_options_are_reported_as_zero_but_not_omitted() {
        let lines = handle_ok(&snapshot_stopped(), &["status"]);
        for key in ["repeat: 0", "random: 0", "single: 0", "consume: 0"] {
            assert!(lines.contains(&key.to_string()), "{key} missing from {lines:?}");
        }
    }

    #[test]
    fn status_designates_the_song_by_its_dense_position_and_its_sparse_index() {
        // The second preset: position 1, index 2. The two are not
        // interchangeable, and confusing them highlights the wrong line.
        let lines = handle_ok(&snapshot_playing(), &["status"]);
        assert!(lines.contains(&"song: 1".to_string()), "{lines:?}");
        assert!(lines.contains(&"songid: 2".to_string()), "{lines:?}");
    }

    #[test]
    fn status_stays_silent_on_a_song_absent_from_the_queue() {
        // A preset outside the queue (a source that announces three
        // entries and plays the seventh): a `song: 6` would designate a
        // position the client will not find in the `playlistinfo` it just
        // read.
        let mut inst = snapshot_playing();
        inst.state.preset = Some(7);
        let lines = handle_ok(&inst, &["status"]);
        assert!(!lines.iter().any(|l| l.starts_with("song")), "{lines:?}");
    }

    #[test]
    fn status_omits_the_time_when_the_position_is_unknown() {
        // A stream where a plugin announces the track's duration without
        // tracking its progress: no invented `elapsed: 0.000`, but the
        // duration stays.
        let mut inst = snapshot_playing();
        inst.state.position_s = None;
        let lines = handle_ok(&inst, &["status"]);
        assert!(!lines.iter().any(|l| l.starts_with("elapsed")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("time")), "{lines:?}");
        assert!(lines.contains(&"duration: 545.000".to_string()), "{lines:?}");
    }

    #[test]
    fn status_announces_a_zero_total_on_a_live_stream() {
        // `time: 12:0`: the position is known, the duration is not. That is
        // what MPD does for streams, and a client reading `time` must not
        // find anything there but two integers.
        let mut inst = snapshot_playing();
        inst.state.track.duration_s = None;
        let lines = handle_ok(&inst, &["status"]);
        assert!(lines.contains(&"time: 12:0".to_string()), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("duration")), "{lines:?}");
    }

    // ------------------------------------------------------------------
    // `currentsong`
    // ------------------------------------------------------------------

    #[test]
    fn currentsong_says_nothing_when_nothing_is_playing() {
        assert_eq!(handle_ok(&snapshot_stopped(), &["currentsong"]), Vec::<String>::new());
    }

    #[test]
    fn currentsong_publishes_the_track_in_the_expected_order() {
        assert_eq!(
            handle_ok(&snapshot_playing(), &["currentsong"]),
            vec![
                "file: ritornello://radio/2",
                "Title: So What",
                "Artist: Miles Davis",
                "Album: Kind of Blue",
                // `Date` is inserted between the album and the duration:
                // the order of the lines is what this test pins down, and
                // a client that reads them by prefix does not care, but
                // pinning it documents the choice.
                "Date: 1959",
                "Time: 545",
                "duration: 545.000",
                "Pos: 1",
                "Id: 2",
            ]
        );
    }

    #[test]
    fn currentsong_omits_unknown_fields_instead_of_leaving_them_empty() {
        // A station with no ICY title: no empty `Title:` line.
        let lines = handle_ok(&snapshot_without_title(), &["currentsong"]);
        assert!(!lines.iter().any(|l| l == "Title: " || l == "Artist: "), "{lines:?}");
        // And no empty field at all, whatever it is.
        assert!(!lines.iter().any(|l| l.ends_with(": ")), "{lines:?}");
    }

    #[test]
    fn currentsong_falls_back_to_the_preset_name_for_lack_of_a_title() {
        // The station's name is the only thing known about a stream with no
        // ICY tag; without this fallback, a client shows only the URI.
        let lines = handle_ok(&snapshot_without_title(), &["currentsong"]);
        assert!(lines.contains(&"Title: Chérie FM".to_string()), "{lines:?}");
    }

    #[test]
    fn currentsong_publishes_the_track_even_while_paused() {
        // MPD keeps a current track while paused: staying silent about it
        // would empty the client's screen as soon as it presses pause.
        let lines = handle_ok(&snapshot_paused(), &["currentsong"]);
        assert!(lines.contains(&"Title: So What".to_string()), "{lines:?}");
    }

    // ------------------------------------------------------------------
    // `playlistinfo` and `plchanges`
    // ------------------------------------------------------------------

    #[test]
    fn playlistinfo_returns_the_whole_queue_with_its_positions_and_indices() {
        assert_eq!(
            handle_ok(&snapshot_without_presets("cd", 2), &["playlistinfo"]),
            vec![
                "file: ritornello://cd/1",
                "Title: 1",
                "Pos: 0",
                "Id: 1",
                "file: ritornello://cd/2",
                "Title: 2",
                "Pos: 1",
                "Id: 2",
            ]
        );
    }

    #[test]
    fn playlistinfo_at_a_position_returns_only_that_entry() {
        assert_eq!(
            handle_ok(&snapshot_without_presets("cd", 3), &["playlistinfo", "2"]),
            vec!["file: ritornello://cd/3", "Title: 3", "Pos: 2", "Id: 3"]
        );
    }

    #[test]
    fn playlistinfo_out_of_bounds_or_non_numeric_is_refused() {
        // A rejection and not an empty response: the client has a stale
        // queue, and a bare `OK` would let it believe there is a hole in
        // the list.
        let inst = snapshot_without_presets("cd", 3);
        for bad in ["3", "-1", "abc", ""] {
            assert_eq!(
                handle_words(&inst, 1, &["playlistinfo", bad]),
                Outcome::Reject("ACK [2@1] {playlistinfo} bad song index".to_string()),
                "position {bad:?} wrongly accepted"
            );
        }
    }

    #[test]
    fn playlistinfo_accepts_a_range_whose_end_is_exclusive() {
        // `playlistinfo [[SONGPOS] | [START:END]]`: a client that windows
        // its queue asks for `0:100`, and an `ACK` on a well-formed request
        // would make it show an empty queue. `1:3` returns two entries, not
        // three, and their `Pos` stays **absolute** — it is the key the
        // client will use to designate the entry afterward.
        assert_eq!(
            handle_ok(&snapshot_without_presets("cd", 4), &["playlistinfo", "1:3"]),
            vec![
                "file: ritornello://cd/2",
                "Title: 2",
                "Pos: 1",
                "Id: 2",
                "file: ritornello://cd/3",
                "Title: 3",
                "Pos: 2",
                "Id: 3",
            ]
        );
    }

    #[test]
    fn playlistinfo_accepts_a_range_with_an_open_end() {
        // `START:` means "to the end", and an end beyond the queue is
        // clamped to the queue rather than overflowing.
        let inst = snapshot_without_presets("cd", 4);
        let open_range = handle_ok(&inst, &["playlistinfo", "2:"]);
        assert_eq!(open_range, handle_ok(&inst, &["playlistinfo", "2:99"]));
        assert_eq!(
            open_range,
            vec![
                "file: ritornello://cd/3",
                "Title: 3",
                "Pos: 2",
                "Id: 3",
                "file: ritornello://cd/4",
                "Title: 4",
                "Pos: 3",
                "Id: 4",
            ]
        );
    }

    #[test]
    fn a_range_that_starts_past_the_end_returns_an_empty_chunk() {
        // Well-formed but pointless: a client that windows its queue may
        // ask for `9:12` right after the queue has shrunk. The answer is
        // "there is nothing there", not an error — unlike a single position
        // out of bounds, which designates a precise entry and stays a
        // rejection.
        let inst = snapshot_without_presets("cd", 3);
        assert_eq!(handle_ok(&inst, &["playlistinfo", "9:12"]), Vec::<String>::new());
        assert_eq!(handle_ok(&inst, &["playlistinfo", "3:3"]), Vec::<String>::new());
        assert!(matches!(
            handle_words(&inst, 0, &["playlistinfo", "9"]),
            Outcome::Reject(_)
        ));
    }

    #[test]
    fn a_reversed_range_is_refused() {
        // No correct client produces `3:1`; accepting it would mask the
        // caller's bug, and MPD refuses it too.
        assert_eq!(
            handle_words(&snapshot_without_presets("cd", 4), 0, &["playlistinfo", "3:1"]),
            Outcome::Reject("ACK [2@0] {playlistinfo} bad song index".to_string())
        );
    }

    #[test]
    fn plchanges_accepts_the_same_window_as_playlistinfo() {
        // `plchanges VERSION [START:END]`: same grammar, same response.
        let inst = snapshot_without_presets("cd", 4);
        assert_eq!(
            handle_ok(&inst, &["plchanges", "6", "0:2"]),
            handle_ok(&inst, &["playlistinfo", "0:2"])
        );
        assert_eq!(
            handle_words(&inst, 0, &["plchanges", "6", "3:1"]),
            Outcome::Reject("ACK [2@0] {plchanges} bad song index".to_string())
        );
    }

    #[test]
    fn plchanges_returns_the_whole_queue_when_the_version_differs() {
        let inst = snapshot_without_presets("cd", 1);
        assert_eq!(
            handle_ok(&inst, &["plchanges", "6"]),
            vec!["file: ritornello://cd/1", "Title: 1", "Pos: 0", "Id: 1"]
        );
    }

    #[test]
    fn plchanges_returns_nothing_when_the_version_is_up_to_date() {
        // The whole point of the command: a client holding the current
        // version does not need to receive 51 lines. `queue_version` is 7
        // in the reference snapshots.
        let inst = snapshot_without_presets("cd", 3);
        assert_eq!(inst.queue_version, 7, "the reference snapshot has changed version");
        assert_eq!(handle_ok(&inst, &["plchanges", "7"]), Vec::<String>::new());
    }

    #[test]
    fn plchanges_without_a_number_is_refused() {
        let inst = snapshot_stopped();
        for words in [vec!["plchanges"], vec!["plchanges", "abc"], vec!["plchanges", "-1"]] {
            assert_eq!(
                handle_words(&inst, 0, &words),
                Outcome::Reject("ACK [2@0] {plchanges} integer expected".to_string()),
                "{words:?} wrongly accepted"
            );
        }
    }

    // ------------------------------------------------------------------
    // The discovery commands
    // ------------------------------------------------------------------

    #[test]
    fn commands_only_announces_what_exists() {
        let lines = handle_ok(&snapshot_stopped(), &["commands"]);
        assert!(lines.contains(&"command: status".to_string()));
        // The counterpart, the one that makes the announcement honest.
        // `search` and `lsinfo` came out of it: they are now handled —
        // empty and well-formed for the first, the sources for the second
        // — because an empty tab beats a tab that crashes. What stays here
        // is what really does not exist: editing the queue, editing the
        // lists, and shutdown.
        for missing in ["delete", "move", "swap", "save", "rm", "playlistadd", "update", "kill"] {
            assert!(!lines.contains(&format!("command: {missing}")), "{missing} wrongly announced");
        }
    }

    #[test]
    fn every_announced_command_is_really_handled() {
        // The counterpart of the previous test, and the only one that
        // prevents `COMMANDS` from drifting from the `match`: a name that
        // is announced but falls into the default rejection shows up here.
        // A rejection for an argument reason (`plchanges` without a
        // version) is legitimate — it is the word `unsupported` that gives
        // away a command that does not exist.
        for name in COMMANDS {
            if let Outcome::Reject(rejection) = handle_words(&snapshot_playing(), 0, &[name]) {
                assert!(!rejection.contains("unsupported"), "{name} announced but not handled: {rejection}");
            }
        }
    }

    #[test]
    fn notcommands_answers_empty() {
        // It lists what the current password **forbids**. There is no
        // password here, so nothing is forbidden by permission: the honest
        // answer is empty, not a rejection that would make an old client
        // that asks for it right after `commands` give up.
        assert_eq!(handle_words(&snapshot_stopped(), 0, &["notcommands"]), Outcome::ok());
    }

    #[test]
    fn commands_is_sorted_and_has_no_duplicate() {
        // Alphabetical order gives clients nothing, but it makes a
        // duplicate or a haphazard insertion visible, the kind Tasks 7 and
        // 13 will make.
        let mut sorted: Vec<&str> = COMMANDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, COMMANDS.to_vec());
    }

    #[test]
    fn tagtypes_only_names_the_four_carried_tags() {
        // `Date` has been part of it since `currentsong` emits it: a
        // client that does not see a tag in `tagtypes` is entitled to
        // never read its line, and the year used to stay invisible on its
        // side because of it.
        assert_eq!(
            handle_ok(&snapshot_stopped(), &["tagtypes"]),
            vec!["tagtype: Artist", "tagtype: Album", "tagtype: Title", "tagtype: Date"]
        );
    }

    #[test]
    fn outputs_announces_a_single_enabled_output() {
        // A disabled output, or no output at all, makes a client that will
        // not insist display "muted".
        assert_eq!(
            handle_ok(&snapshot_stopped(), &["outputs"]),
            vec!["outputid: 0", "outputname: default", "outputenabled: 1"]
        );
    }

    #[test]
    fn stats_counts_the_queue_and_admits_knowing_nothing_of_the_rest() {
        // `uptime: 0` is deliberate: getting it right would require
        // remembering a start instant, hence a clock in a module that has
        // none.
        let lines = handle_ok(&snapshot_without_presets("cd", 12), &["stats"]);
        assert!(lines.contains(&"songs: 12".to_string()), "{lines:?}");
        assert!(lines.contains(&"uptime: 0".to_string()), "{lines:?}");
        assert!(lines.contains(&"db_update: 0".to_string()), "{lines:?}");
    }

    #[test]
    fn decoders_and_urlhandlers_answer_a_bare_ok_but_do_answer() {
        // Present and empty: an unknown command at connection time can
        // make a client give up before it draws a screen.
        for name in ["decoders", "urlhandlers"] {
            assert_eq!(handle_words(&snapshot_stopped(), 0, &[name]), Outcome::ok(), "{name}");
        }
    }

    #[test]
    fn ping_password_and_close_ask_nothing_of_the_device() {
        let inst = snapshot_stopped();
        assert_eq!(handle_words(&inst, 0, &["ping"]), Outcome::ok());
        // Without checking anything, and even without an argument: there
        // is no password, so nothing to check and nothing to reject.
        assert_eq!(handle_words(&inst, 0, &["password", "secret"]), Outcome::ok());
        assert_eq!(handle_words(&inst, 0, &["password"]), Outcome::ok());
        assert_eq!(handle_words(&inst, 0, &["close"]), Outcome::Close);
    }

    #[test]
    fn no_read_only_command_emits_a_command_toward_the_core() {
        // Playback alone is really only playback: a `status` that acted on
        // the device would be an invisible side effect, and clients send
        // several per second.
        let inst = snapshot_playing();
        let queries: [&[&str]; 14] = [
            &["status"],
            &["currentsong"],
            &["playlistinfo"],
            &["playlistinfo", "0"],
            &["playlistinfo", "0:2"],
            &["plchanges", "0"],
            &["commands"],
            &["notcommands"],
            &["tagtypes"],
            &["outputs"],
            &["stats"],
            &["decoders"],
            &["urlhandlers"],
            &["ping"],
        ];
        for words in queries {
            match handle_words(&inst, 0, words) {
                Outcome::Reply { cmds, .. } => {
                    assert!(cmds.is_empty(), "{words:?} emitted {cmds:?}");
                }
                other => panic!("expected Reply for {words:?}, got {other:?}"),
            }
        }
    }

    // ------------------------------------------------------------------
    // `idle` / `noidle`
    // ------------------------------------------------------------------

    #[test]
    fn idle_without_an_argument_waits_on_the_four_subsystems() {
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["idle"]),
            Outcome::Wait(vec![
                Subsystem::Player,
                Subsystem::Mixer,
                Subsystem::Playlist,
                Subsystem::StoredPlaylist
            ])
        );
    }

    #[test]
    fn idle_keeps_only_the_named_subsystems_in_order_and_without_duplicates() {
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["idle", "mixer", "player", "mixer"]),
            Outcome::Wait(vec![Subsystem::Mixer, Subsystem::Player])
        );
    }

    #[test]
    fn a_word_outside_mpds_vocabulary_is_refused() {
        // A word MPD itself does not know: a client that misspelled its
        // subsystem would stay silent forever, which is far harder to
        // diagnose than an `ACK`.
        for word in ["jukebox", "Player", "stored_playlists", ""] {
            assert_eq!(
                handle_words(&snapshot_stopped(), 2, &["idle", word]),
                Outcome::Reject("ACK [2@2] {idle} unrecognized idle event".to_string()),
                "{word:?} should have been refused"
            );
        }
    }

    #[test]
    fn idle_accepts_mpds_subsystems_we_never_emit() {
        // The defect seen from the other side: any client built on
        // libmpdclient's `mpd_send_idle_mask` sends an explicit list, in
        // practice `database update stored_playlist playlist player mixer
        // output options`. Rejecting a **legal** word would earn it an
        // `ACK` on its first `idle`, hence a loop or giving up.
        let inst = snapshot_stopped();
        let words = [
            "idle",
            "database",
            "update",
            "stored_playlist",
            "playlist",
            "player",
            "mixer",
            "output",
            "options",
        ];
        assert_eq!(
            handle_words(&inst, 0, &words),
            Outcome::Wait(vec![Subsystem::StoredPlaylist, Subsystem::Playlist, Subsystem::Player, Subsystem::Mixer])
        );
        // And the four other names of the vocabulary, the ones no current
        // client sends but MPD knows.
        for word in ["partition", "sticker", "subscription", "message", "neighbor", "mount"] {
            assert_eq!(
                handle_words(&inst, 0, &["idle", word]),
                Outcome::Wait(Vec::new()),
                "{word} should be accepted then dropped"
            );
        }
    }

    #[test]
    fn a_wait_on_a_subsystem_never_emitted_is_empty_and_not_immediate() {
        // `Wait(vec![])` is not `OK`: the client asked to be told about a
        // change that will never arrive, and waiting forever is the
        // correct MPD answer. The contract is noted on the variant,
        // because it is Task 8 that could betray it by treating the empty
        // list as a bare `OK`.
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["idle", "database"]),
            Outcome::Wait(Vec::new())
        );
    }

    #[tokio::test]
    async fn a_mixed_list_keeps_the_wakeup_of_the_subsystem_we_emit() {
        // `idle database mixer`: `database` is accepted then dropped, and
        // that drop must not carry `mixer`'s wakeup away with it. Verified
        // end to end against the shared state, and not merely against the
        // `Outcome`'s payload.
        let outcome = handle_words(&snapshot_stopped(), 0, &["idle", "database", "mixer"]);
        let Outcome::Wait(subsystems) = outcome else {
            panic!("expected Wait, got {outcome:?}");
        };
        assert_eq!(subsystems, vec![Subsystem::Mixer]);

        let shared = SharedState::default();
        let seen = shared.versions().await;
        shared.apply_state(PlayerState { volume: 55, ..Default::default() }).await;
        // No clock margin: the change has **already** happened, so `wait`
        // returns through its up-front comparison without ever sleeping.
        // If `mixer` had been dropped along with `database`, the list
        // would be empty and this test **would hang** — the failure is
        // clear-cut, matching `state.rs`'s test idiom.
        assert_eq!(shared.wait(&subsystems, seen).await.moved, vec![Subsystem::Mixer]);
    }

    #[test]
    fn noidle_returns_control_without_waiting() {
        assert_eq!(handle_words(&snapshot_stopped(), 0, &["noidle"]), Outcome::Cancel);
    }

    // ------------------------------------------------------------------
    // `play` / `playid`
    // ------------------------------------------------------------------

    #[test]
    fn position_to_index_picks_the_rank_and_not_the_index_minus_one() {
        // The shift that costs dearly: on indices 1, 5, 99, rank 1
        // (0-based, second entry) must return 5 — not 2 (the "rank plus
        // one"), nor any other calculation derived from the position. A
        // hand-built queue: see the limit documented on
        // `snapshot_with_presets`, `queue` cannot yet synthesize a sparse
        // sequence.
        let file = vec![
            Entry { index: 1, name: "FIP".into() },
            Entry { index: 5, name: "France Inter".into() },
            Entry { index: 99, name: "Nova".into() },
        ];
        assert_eq!(position_to_index(&file, 0), Some(1));
        assert_eq!(position_to_index(&file, 1), Some(5));
        assert_eq!(position_to_index(&file, 2), Some(99));
        assert_eq!(position_to_index(&file, 3), None, "past the queue");
    }

    #[test]
    fn index_exists_checks_membership_and_not_the_bound() {
        // 2 is indeed below the queue's maximum (5), but absent: a
        // `playid 2` must be rejected, which a bound comparison would
        // wrongly let through once the queue is sparse (Task 13).
        let file = vec![Entry { index: 1, name: "FIP".into() }, Entry { index: 5, name: "France Inter".into() }];
        assert!(index_exists(&file, 5));
        assert!(!index_exists(&file, 2), "2 is under the maximum (5) but absent from the queue");
    }

    #[test]
    fn play_with_a_position_selects_the_entry_of_that_rank() {
        // The end-to-end path, within the limits of what
        // `snapshot_with_presets` can build today (see its doc): a dense
        // queue where the rank is verified by going through `handle`, not
        // by a direct call to `position_to_index`.
        let inst = snapshot_with_presets("radio", &[(1, "one"), (2, "two"), (3, "three")]);
        assert_eq!(cmds(&inst, &["play", "0"]), vec![Command::Select(1)]);
        assert_eq!(cmds(&inst, &["play", "2"]), vec![Command::Select(3)]);
    }

    #[test]
    fn playid_checks_existence_through_handle() {
        let inst = snapshot_with_presets("radio", &[(1, "one"), (2, "two")]);
        assert_eq!(cmds(&inst, &["playid", "2"]), vec![Command::Select(2)]);
    }

    #[test]
    fn play_out_of_bounds_is_refused_and_emits_nothing() {
        let inst = snapshot_with_presets("radio", &[(1, "FIP")]);
        assert!(matches!(handle(&inst, 0, &["play".into(), "7".into()], MAX_CHUNK), Outcome::Reject(_)));
    }

    #[test]
    fn playid_of_a_missing_index_is_refused() {
        let inst = snapshot_with_presets("radio", &[(1, "FIP")]);
        assert!(matches!(handle(&inst, 0, &["playid".into(), "9".into()], MAX_CHUNK), Outcome::Reject(_)));
    }

    #[test]
    fn play_and_playid_with_a_non_numeric_argument_are_refused() {
        // `play` without an argument is *not* a rejection (it is the Play
        // key, see the next test); it is only a non-numeric argument, or
        // the absence of `playid`'s only argument, that must be.
        let inst = snapshot_with_presets("radio", &[(1, "FIP")]);
        for words in [vec!["play", "abc"], vec!["playid"], vec!["playid", "abc"]] {
            assert!(matches!(handle_words(&inst, 0, &words), Outcome::Reject(_)), "{words:?}");
        }
    }

    #[test]
    fn play_without_an_argument_relaunches_what_was_loaded() {
        // The Play key, not a selection.
        let inst = snapshot_stopped();
        assert_eq!(cmds(&inst, &["play"]), vec![Command::PlayPause]);
    }

    // ------------------------------------------------------------------
    // `pause`
    // ------------------------------------------------------------------

    #[test]
    fn pause_emits_nothing_when_the_state_is_already_the_one_requested() {
        // This is what closes the race: a `pause 1` on a playback already
        // paused must not relaunch it.
        let inst = snapshot_paused();
        assert_eq!(cmds(&inst, &["pause", "1"]), Vec::<Command>::new());
        assert_eq!(cmds(&inst, &["pause", "0"]), vec![Command::PlayPause]);
    }

    #[test]
    fn pause_without_an_argument_toggles() {
        assert_eq!(cmds(&snapshot_playing(), &["pause"]), vec![Command::PlayPause]);
    }

    #[test]
    fn pause_on_a_stopped_player_never_emits_anything() {
        // A rule distinct from the state/target comparison above:
        // `PlayPause` while stopped would start a playback that neither
        // the source nor this plugin know anything about (see
        // `SharedState::acknowledge_optimistic`), which is not what a
        // client asked for by pressing "pause".
        let inst = snapshot_stopped();
        assert_eq!(cmds(&inst, &["pause"]), Vec::<Command>::new());
        assert_eq!(cmds(&inst, &["pause", "0"]), Vec::<Command>::new());
        assert_eq!(cmds(&inst, &["pause", "1"]), Vec::<Command>::new());
    }

    #[test]
    fn pause_with_an_invalid_argument_is_refused_even_while_stopped() {
        // Argument validation happens before the stopped guard: a
        // malformed `pause 2` must stay an `ACK`, not be silently
        // swallowed by "nothing to do while stopped".
        assert!(matches!(
            handle_words(&snapshot_stopped(), 0, &["pause", "2"]),
            Outcome::Reject(_)
        ));
    }

    // ------------------------------------------------------------------
    // `setvol` / `volume`
    // ------------------------------------------------------------------

    #[test]
    fn setvol_is_clamped_and_refuses_out_of_interval() {
        let inst = snapshot_stopped();
        assert_eq!(cmds(&inst, &["setvol", "40"]), vec![Command::SetVolume(40)]);
        assert!(matches!(handle(&inst, 0, &["setvol".into(), "101".into()], MAX_CHUNK), Outcome::Reject(_)));
        assert!(matches!(handle(&inst, 0, &["setvol".into(), "abc".into()], MAX_CHUNK), Outcome::Reject(_)));
        assert!(matches!(handle(&inst, 0, &["setvol".into()], MAX_CHUNK), Outcome::Reject(_)));
    }

    #[test]
    fn setvol_zero_is_not_translated_into_a_mute() {
        // That would be guessing: `Mute` toggles, `SetVolume(0)` sets.
        // Translating it would make a client raising the volume land on a
        // sound still cut.
        assert_eq!(cmds(&snapshot_at_volume(65), &["setvol", "0"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn setvol_above_zero_lifts_the_mute() {
        // **The only path an MPD client has to turn the sound back on.**
        // `status` publishes `volume: 0` as soon as the device is muted, so
        // the client raises its slider, `SetVolume(40)` goes out, the
        // volume changes — and the sound stayed cut, with no way at all to
        // fix it from the phone.
        // The order is pinned here because the test compares a `Vec`, not
        // because it would change the result: both orders leave the device
        // unmuted at 40 (the core resets no volume while unmuting, see
        // `setvol`). What it preserves is the interval — the sound already
        // comes back at 40 instead of passing through the memorized level.
        assert_eq!(
            cmds(&snapshot_muted(65), &["setvol", "40"]),
            vec![Command::SetVolume(40), Command::Mute]
        );
    }

    #[test]
    fn setvol_emits_no_mute_when_the_sound_is_not_cut() {
        // The other direction, and it is essential: `Command::Mute` is a
        // **toggle**, so emitting it unconditionally would cut the sound of
        // a client that just raised its own. Same conditional shape as
        // `pause 0`/`pause 1` against `playback`.
        assert_eq!(cmds(&snapshot_at_volume(65), &["setvol", "40"]), vec![Command::SetVolume(40)]);
    }

    #[test]
    fn setvol_zero_on_a_muted_device_lifts_nothing() {
        // The edge case where both rules meet: setting zero is not "asking
        // to hear something", so nothing to lift — and lifting here would
        // turn the sound back on for a client asking for silence.
        assert_eq!(cmds(&snapshot_muted(65), &["setvol", "0"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn relative_volume_also_lifts_the_mute() {
        // Same gesture, same rule: `volume` is deprecated but it is the old
        // half of the client fleet, and leaving it without a way out would
        // make the sound's return depend on the client's age. The
        // computation starts from the **memorized** volume (65) and not
        // from the zero `status` publishes.
        assert_eq!(
            cmds(&snapshot_muted(65), &["volume", "+10"]),
            vec![Command::SetVolume(75), Command::Mute]
        );
        // And a rewind that reaches zero lifts nothing, like `setvol 0`.
        assert_eq!(cmds(&snapshot_muted(5), &["volume", "-10"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn volume_is_relative_and_clamped_on_the_current_volume() {
        // A deprecated command but still emitted. Clamped here, not left to
        // overflow.
        let inst = snapshot_at_volume(95);
        assert_eq!(cmds(&inst, &["volume", "+10"]), vec![Command::SetVolume(100)]);
        assert_eq!(cmds(&snapshot_at_volume(3), &["volume", "-10"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn volume_at_i16s_bounds_is_clamped_without_overflowing() {
        // `delta` is parsed as-is from the client's argument, so it can be
        // anywhere in `±32767`: adding that maximum to even a low current
        // volume overflows `i16` before `.clamp` can act. A panic in
        // debug/test (overflow checks are on by default in this profile),
        // a wrong value in release — on a port open to the local network,
        // without authentication. The three starting volumes (low, zero,
        // high) cover both directions of the overflow.
        assert_eq!(
            cmds(&snapshot_at_volume(1), &["volume", "32767"]),
            vec![Command::SetVolume(100)]
        );
        assert_eq!(
            cmds(&snapshot_at_volume(0), &["volume", "32767"]),
            vec![Command::SetVolume(100)]
        );
        assert_eq!(
            cmds(&snapshot_at_volume(50), &["volume", "-32768"]),
            vec![Command::SetVolume(0)]
        );
    }

    // ------------------------------------------------------------------
    // `seek` / `seekid` / `seekcur`
    // ------------------------------------------------------------------

    #[test]
    fn seekcur_resolves_the_relative_before_emitting_an_absolute() {
        // `Command` only carries an absolute positioning: the resolution
        // happens here.
        let inst = snapshot_at_position(30);
        assert_eq!(cmds(&inst, &["seekcur", "+10"]), vec![Command::SeekTo(40)]);
        assert_eq!(cmds(&inst, &["seekcur", "-10"]), vec![Command::SeekTo(20)]);
        assert_eq!(cmds(&inst, &["seekcur", "12.5"]), vec![Command::SeekTo(12)]);
        // A rewind larger than the position does not produce a negative time.
        assert_eq!(cmds(&snapshot_at_position(3), &["seekcur", "-10"]), vec![Command::SeekTo(0)]);
    }

    #[test]
    fn a_relative_seekcur_with_no_known_position_is_refused() {
        // Resolving a relative time with no starting point would invent a
        // time: neither 0 nor any other silent value.
        let inst = snapshot_stopped();
        assert_eq!(inst.state.position_s, None, "the reference snapshot has no position");
        assert!(matches!(
            handle_words(&inst, 0, &["seekcur", "+10"]),
            Outcome::Reject(_)
        ));
    }

    #[test]
    fn seekcur_without_an_argument_or_non_numeric_is_refused() {
        let inst = snapshot_at_position(10);
        for words in [vec!["seekcur"], vec!["seekcur", "abc"], vec!["seekcur", "+abc"]] {
            assert!(matches!(handle_words(&inst, 0, &words), Outcome::Reject(_)), "{words:?}");
        }
    }

    #[test]
    fn seek_and_seekid_ignore_their_first_argument() {
        // `Command::SeekTo` cannot change track at the same time; MPD only
        // sends `seek` about what is already playing anyway.
        let inst = snapshot_at_position(0);
        assert_eq!(cmds(&inst, &["seek", "0", "42"]), vec![Command::SeekTo(42)]);
        assert_eq!(cmds(&inst, &["seekid", "1", "42"]), vec![Command::SeekTo(42)]);
    }

    #[test]
    fn seek_normalizes_a_redundant_leading_plus_sign_in_the_time() {
        // `seek`/`seekid` stay absolute: a leading `+` is only a number
        // sign there like any other (`absolute_time` does not distinguish
        // the relative form, reserved for `seekcur`), so `+5` and `5` must
        // produce exactly the same command.
        let inst = snapshot_at_position(0);
        assert_eq!(cmds(&inst, &["seek", "0", "+5"]), cmds(&inst, &["seek", "0", "5"]));
    }

    #[test]
    fn non_finite_times_are_refused_and_not_swallowed() {
        // `inf` and `nan` parse into `f64`, and without a guard `seek 0
        // inf` used to return `SeekTo(u32::MAX)` while `seek 0 nan` used to
        // return `SeekTo(0)` — both **silently**, against the rule this
        // module states: a non-numeric argument is an `ACK 2`, never a
        // silent defect. Same class as `volume`'s `i16` overflow, two
        // meters away.
        //
        // All three of the protocol's forms are covered, because
        // `seekcur`'s relative form has its own parsing and hence its own
        // hole.
        let inst = snapshot_at_position(30);
        for words in [
            vec!["seek", "0", "inf"],
            vec!["seek", "0", "-inf"],
            vec!["seek", "0", "nan"],
            vec!["seek", "0", "NaN"],
            vec!["seekid", "1", "inf"],
            vec!["seekcur", "inf"],
            vec!["seekcur", "nan"],
            vec!["seekcur", "+inf"],
            vec!["seekcur", "-inf"],
            vec!["seekcur", "+nan"],
        ] {
            assert!(
                matches!(handle_words(&inst, 0, &words), Outcome::Reject(_)),
                "{words:?} must be refused, not swallowed"
            );
        }
        // And the closest legitimate form stays accepted: "infinity" is not
        // a number, but `1e9` is one.
        assert_eq!(cmds(&inst, &["seek", "0", "1000000"]), vec![Command::SeekTo(1_000_000)]);
    }

    #[test]
    fn seek_and_seekid_without_a_time_are_refused() {
        let inst = snapshot_at_position(0);
        assert!(matches!(handle_words(&inst, 0, &["seek", "0"]), Outcome::Reject(_)));
        assert!(matches!(handle_words(&inst, 0, &["seekid", "1"]), Outcome::Reject(_)));
    }

    // ------------------------------------------------------------------
    // The simple keys
    // ------------------------------------------------------------------

    #[test]
    fn the_simple_keys_pass_through_unchanged() {
        let inst = snapshot_playing();
        assert_eq!(cmds(&inst, &["next"]), vec![Command::Next]);
        assert_eq!(cmds(&inst, &["previous"]), vec![Command::Prev]);
        assert_eq!(cmds(&inst, &["stop"]), vec![Command::Stop]);
    }

    // ------------------------------------------------------------------
    // Stored playlists: `listplaylists`, `listplaylistinfo`, `load`
    // ------------------------------------------------------------------

    #[test]
    fn listplaylists_names_one_list_per_source() {
        let inst = snapshot_with_catalog(&["radio", "cd", "files"]);
        let lines = handle_ok(&inst, &["listplaylists"]);
        assert_eq!(lines.iter().filter(|l| l.starts_with("playlist: ")).count(), 3);
        assert!(lines.contains(&"playlist: radio".to_string()), "{lines:?}");
    }

    #[test]
    fn listplaylists_keeps_the_catalogs_order() {
        // The order received is `SourceCycle`'s cycling order, hence the
        // one the user sees on their remote: sorting it alphabetically
        // would lose information the client can display.
        let inst = snapshot_with_catalog(&["radio", "cd", "files"]);
        let names: Vec<String> = handle_ok(&inst, &["listplaylists"])
            .into_iter()
            .filter_map(|l| l.strip_prefix("playlist: ").map(str::to_string))
            .collect();
        assert_eq!(names, vec!["radio", "cd", "files"]);
    }

    #[test]
    fn listplaylists_returns_one_date_per_entry() {
        // `Last-Modified` is emitted and not omitted: clients read it, and
        // its absence trips them up. The value is a constant — no date
        // exists on the device side, and a clock would suggest a change on
        // every re-read.
        let inst = snapshot_with_catalog(&["radio", "cd"]);
        assert_eq!(
            handle_ok(&inst, &["listplaylists"]),
            vec![
                "playlist: radio",
                "Last-Modified: 1970-01-01T00:00:00Z",
                "playlist: cd",
                "Last-Modified: 1970-01-01T00:00:00Z",
            ]
        );
    }

    #[test]
    fn listplaylists_is_empty_before_the_first_catalog() {
        // A bare `OK`, not a rejection: the plugin knows no source yet, and
        // that is the truth of that instant. The client will re-read after
        // its wakeup on `stored_playlist`.
        assert_eq!(handle_words(&snapshot_stopped(), 0, &["listplaylists"]), Outcome::ok());
    }

    #[test]
    fn listplaylistinfo_returns_the_real_names() {
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        let lines = handle_ok(&inst, &["listplaylistinfo", "radio"]);
        assert!(lines.contains(&"Title: FIP".to_string()), "{lines:?}");
        assert!(lines.contains(&"Title: Nova".to_string()), "{lines:?}");
        // And the URI carries the **sparse** index, not a rank: it is the
        // stable key with which the client will find the entry again in
        // the queue.
        assert!(lines.contains(&"file: ritornello://radio/5".to_string()), "{lines:?}");
    }

    #[test]
    fn listplaylistinfo_queries_a_source_that_is_not_playing() {
        // The case that motivated the workaround for the core-side guard:
        // the sources catalog describes every source, and a client can
        // read the radio's list while a disc is spinning.
        let inst = snapshot_active_on("cd", &[("radio", &[(1, "FIP")])]);
        assert_eq!(inst.state.source, "cd", "the fixture must indeed be playing something else");
        assert!(handle_ok(&inst, &["listplaylistinfo", "radio"])
            .contains(&"Title: FIP".to_string()));
    }

    #[test]
    fn listplaylistinfo_emits_neither_pos_nor_id() {
        // `Pos` and `Id` designate an entry of the **queue**, and a stored
        // playlist is not loaded: emitting them would give a client
        // positions it would never find again in its `playlistinfo`. MPD
        // does not publish them here either.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        let lines = handle_ok(&inst, &["listplaylistinfo", "radio"]);
        assert!(
            !lines.iter().any(|l| l.starts_with("Pos: ") || l.starts_with("Id: ")),
            "{lines:?}"
        );
    }

    #[test]
    fn listplaylistinfo_of_the_active_source_without_a_list_says_the_same_as_the_queue() {
        // The cd that is playing: it cannot enumerate, but `preset_count`
        // does describe its twelve tracks, and the two responses must
        // agree — a client comparing the stored playlist to the queue must
        // not see two different devices.
        let inst = Snapshot {
            sources_catalog: SourcesCatalog { sources: vec![make_source_catalog("cd", &[])] },
            ..snapshot_without_presets("cd", 12)
        };
        let lines = handle_ok(&inst, &["listplaylistinfo", "cd"]);
        assert_eq!(lines.len(), 24, "two lines per track: {lines:?}");
        assert!(lines.contains(&"Title: 12".to_string()), "{lines:?}");
    }

    #[test]
    fn listplaylistinfo_of_an_inactive_source_without_a_list_is_empty_and_not_guessed() {
        // `preset_count` only describes the **active** source: guessing the
        // track count of a disc that is not playing would be an
        // invention, and a well-formed empty list is the honest answer.
        let inst = snapshot_active_on("radio", &[("radio", &[(1, "FIP")]), ("cd", &[])]);
        assert_eq!(handle_words(&inst, 0, &["listplaylistinfo", "cd"]), Outcome::ok());
    }

    #[test]
    fn an_unknown_playlist_name_is_an_ack_50() {
        let inst = snapshot_with_catalog(&["radio"]);
        assert_eq!(
            handle_words(&inst, 0, &["listplaylistinfo", "nonsense"]),
            Outcome::Reject("ACK [50@0] {listplaylistinfo} no such playlist".to_string())
        );
    }

    #[test]
    fn a_missing_playlist_name_is_an_ack_2_and_not_a_50() {
        // The missing name is not a nonexistent list but faulty syntax:
        // `ACK 2`, with the command's index in its list.
        let inst = snapshot_with_catalog(&["radio"]);
        for cmd in ["listplaylistinfo", "load"] {
            assert_eq!(
                handle_words(&inst, 3, &[cmd]),
                Outcome::Reject(format!("ACK [2@3] {{{cmd}}} wrong number of arguments"))
            );
        }
    }

    #[test]
    fn load_switches_source() {
        let inst = snapshot_with_catalog(&["radio", "cd"]);
        assert_eq!(cmds(&inst, &["load", "cd"]), vec![Command::SelectSource("cd".into())]);
    }

    #[test]
    fn load_of_an_unknown_name_is_refused_and_emits_nothing() {
        // The plugin only offers names received from the sources catalog:
        // it is the one that refuses, not the core silently
        // (`SelectSource` of an unknown name is ignored there, and an `OK`
        // followed by nothing would be the worst possible answer for a
        // client, which would wait for a queue change that never arrives).
        let inst = snapshot_with_catalog(&["radio"]);
        assert_eq!(
            handle_words(&inst, 0, &["load", "nonsense"]),
            Outcome::Reject("ACK [50@0] {load} no such playlist".to_string())
        );
    }

    #[test]
    fn load_of_the_already_active_source_switches_anyway() {
        // No trick here: it is the core that knows whether `SelectSource`
        // on the current source restarts or does nothing, and guessing on
        // its behalf would silently swallow the `load` of a client that
        // just lost its state.
        let inst = snapshot_with_catalog(&["radio", "cd"]);
        assert_eq!(cmds(&inst, &["load", "radio"]), vec![Command::SelectSource("radio".into())]);
    }

    #[test]
    fn the_three_playlist_commands_are_now_announced() {
        // Task 7 deliberately kept them silent: `load` refused every name,
        // for lack of a sources catalog, and announcing it would have
        // broken the honesty `commands` promises. The sources catalog is
        // here, they work, they declare themselves.
        let lines = handle_ok(&snapshot_with_catalog(&["radio"]), &["commands"]);
        for name in ["load", "listplaylists", "listplaylistinfo"] {
            assert!(COMMANDS.contains(&name), "{name} missing from COMMANDS");
            assert!(lines.contains(&format!("command: {name}")), "{name} not announced");
        }
    }

    // ------------------------------------------------------------------
    // The rejections
    // ------------------------------------------------------------------

    #[test]
    fn an_unknown_command_is_refused_with_its_list_index() {
        let inst = snapshot_stopped();
        assert_eq!(
            handle(&inst, 3, &["nonsense".to_string()], MAX_CHUNK),
            Outcome::Reject("ACK [5@3] {nonsense} unsupported".to_string())
        );
    }

    #[test]
    fn the_write_commands_are_refused_one_by_one() {
        // They must be explicitly, not by default: it is the list the doc
        // promises, and a future `add` accidentally handled would show up
        // here. The list is that of the spec's § "What the plugin does
        // not do".
        //
        // **The six library queries came out of it** (`lsinfo`, `listall`,
        // `listallinfo`, `search`, `find`, `list`, `count`): they now
        // answer, empty and well-formed for lack of a database — except
        // `lsinfo`, which returns the sources. The rejection was a visible
        // defect on the client's side, whose tab showed an error where an
        // empty list would have shown nothing. What stays here is
        // editing, which makes no sense on this device, and it alone.
        for cmd in [
            "update",
            "delete",
            "deleteid",
            "move",
            "swap",
            "shuffle",
            "save",
            "rm",
            "rename",
            "playlistadd",
            "playlistdelete",
            "repeat",
            "random",
            "single",
            "consume",
            "crossfade",
            "replay_gain_mode",
            "enableoutput",
            "disableoutput",
            "subscribe",
            "sendmessage",
            "kill",
            // `albumart`, `readpicture` then `binarylimit` used to be here
            // and no longer are: they are now handled, and this is
            // precisely the list that had to change — removing it from
            // here is the "handled ⊆ COMMANDS" half of the pair of
            // invariants, the other being
            // `every_announced_command_is_really_handled`.
        ] {
            assert_eq!(
                handle_words(&snapshot_stopped(), 0, &[cmd]),
                Outcome::Reject(format!("ACK [5@0] {{{cmd}}} unsupported")),
                "{cmd} should be refused"
            );
        }
    }

    // ------------------------------------------------------------------
    // The library: what a client can browse
    // ------------------------------------------------------------------

    #[test]
    fn add_of_a_uri_of_the_active_source_plays_that_entry() {
        // **The gesture the owner reported broken**: touching a track in a
        // stored playlist used to return `ACK 5`. On the already-active
        // source, a single command suffices.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        assert_eq!(cmds(&inst, &["add", "ritornello://radio/5"]), vec![Command::Select(5)]);
    }

    #[test]
    fn add_of_another_source_chooses_it_before_playing() {
        // Two commands, in this order: the queue *is* the source's list, so
        // playing an entry from elsewhere means changing source first.
        let inst = snapshot_active_on("radio", &[("radio", &[(1, "FIP")]), ("cd", &[(2, "Track 2")])]);
        assert_eq!(
            cmds(&inst, &["add", "ritornello://cd/2"]),
            vec![Command::SelectSource("cd".into()), Command::Select(2)]
        );
    }

    #[test]
    fn addid_returns_the_id_the_way_mpd_does() {
        // The only difference between the two commands. Its possible
        // position is ignored: there is no queue to insert into.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        assert_eq!(handle_ok(&inst, &["addid", "ritornello://radio/5", "0"]), vec!["Id: 5"]);
        assert_eq!(
            cmds(&inst, &["addid", "ritornello://radio/5", "0"]),
            vec![Command::Select(5)]
        );
    }

    #[test]
    fn add_of_a_uri_that_designates_nothing_is_refused() {
        // Including an index **within bounds but absent** from a sparse
        // table: same rule as `playid`, a bound is not enough.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        for uri in [
            "ritornello://radio/3",     // hole in the sparse sequence
            "ritornello://unknown/1",   // source missing from the sources catalog
            "/music/track.flac",        // not one of our URIs
            "ritornello://radio",       // truncated
            "ritornello:///1",          // empty source
        ] {
            assert_eq!(
                handle_words(&inst, 0, &["add", uri]),
                Outcome::Reject("ACK [50@0] {add} No such song".to_string()),
                "{uri} wrongly accepted"
            );
        }
        assert_eq!(
            handle_words(&inst, 0, &["add"]),
            Outcome::Reject("ACK [2@0] {add} wrong number of arguments".to_string())
        );
    }

    #[test]
    fn clear_is_accepted_without_doing_anything() {
        // There is no queue to empty. An `ACK` would interrupt the
        // `clear`/`add`/`play` list a client sends to play a track, so the
        // rejection would cost exactly the feature just added.
        let inst = snapshot_with_presets("radio", &[(1, "FIP")]);
        assert_eq!(handle_ok(&inst, &["clear"]), Vec::<String>::new());
        assert_eq!(cmds(&inst, &["clear"]), Vec::<Command>::new());
    }

    #[test]
    fn lsinfo_at_the_root_returns_the_sources_like_listplaylists() {
        // A client's file browser must show what the device has: its
        // sources. The two commands must answer exactly the same thing
        // from the root, otherwise a client would see two different
        // libraries depending on the tab.
        let inst = snapshot_with_catalog(&["radio", "cd", "files"]);
        let expected = handle_ok(&inst, &["listplaylists"]);
        assert!(expected.contains(&"playlist: radio".to_string()));
        for root in [vec!["lsinfo"], vec!["lsinfo", ""], vec!["lsinfo", "/"]] {
            assert_eq!(handle_ok(&inst, &root), expected, "{root:?}");
        }
    }

    #[test]
    fn lsinfo_of_a_source_returns_its_entries_like_listplaylistinfo() {
        // Descending into a source must give its presets, and the same
        // ones as the stored-playlist command: it is the same content seen
        // through two paths, and letting them diverge would play something
        // other than what was touched on screen.
        let inst = snapshot_with_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        assert_eq!(
            handle_ok(&inst, &["lsinfo", "radio"]),
            handle_ok(&inst, &["listplaylistinfo", "radio"])
        );
    }

    #[test]
    fn lsinfo_of_an_unknown_name_is_refused() {
        // An empty `OK` would suggest a genuinely empty folder.
        assert_eq!(
            handle_words(&snapshot_with_catalog(&["radio"]), 0, &["lsinfo", "music"]),
            Outcome::Reject("ACK [50@0] {lsinfo} No such directory".to_string())
        );
    }

    #[test]
    fn the_database_queries_answer_empty_and_well_formed() {
        // **Empty rather than refused**, and that is the fix: a client's
        // "Albums" tab used to receive `ACK 5` and show an error, where an
        // empty response shows nothing. There is no database here, and
        // saying so with an empty list is honest; saying so with a
        // rejection was simply unreadable.
        let inst = snapshot_playing();
        for words in [
            vec!["list", "album"],
            vec!["listall"],
            vec!["listallinfo"],
            vec!["listfiles"],
            vec!["find", "album", "Kind of Blue"],
            vec!["search", "any", "miles"],
        ] {
            assert_eq!(handle_ok(&inst, &words), Vec::<String>::new(), "{words:?}");
        }
        // `count` returns its two fields: clients read them without testing
        // for them.
        assert_eq!(
            handle_ok(&inst, &["count", "album", "Kind of Blue"]),
            vec!["songs: 0".to_string(), "playtime: 0".to_string()]
        );
    }

    #[test]
    fn a_search_without_a_filter_stays_refused() {
        // A truncated request must be learned, otherwise the client
        // believes its search returned nothing.
        for cmd in ["find", "search"] {
            assert_eq!(
                handle_words(&snapshot_playing(), 0, &[cmd]),
                Outcome::Reject(format!("ACK [2@0] {{{cmd}}} too few arguments"))
            );
        }
    }

    #[test]
    fn getvol_says_the_same_volume_as_status() {
        // Two volumes contradicting each other would be an invisible
        // defect until the day a client reads both — mute included.
        for inst in [snapshot_playing(), snapshot_muted(65)] {
            let from_status = handle_ok(&inst, &["status"])[0].clone();
            assert_eq!(handle_ok(&inst, &["getvol"]), vec![from_status]);
        }
    }

    #[test]
    fn binarylimit_is_clamped_on_both_sides_rather_than_refused() {
        // The upper bound is a decision of ours and not a protocol rule:
        // refusing a client that asks for more than we want to serve would
        // fail the connection, whereas a smaller chunk than requested is
        // always legal.
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["binarylimit", "16384"]),
            Outcome::BinaryLimit(16 * 1024)
        );
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["binarylimit", "1048576"]),
            Outcome::BinaryLimit(MAX_CHUNK_CAP)
        );
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["binarylimit", "1"]),
            Outcome::BinaryLimit(MIN_CHUNK)
        );
        assert_eq!(
            handle_words(&snapshot_stopped(), 0, &["binarylimit", "many"]),
            Outcome::Reject("ACK [2@0] {binarylimit} integer expected".to_string())
        );
    }

    #[test]
    fn an_empty_line_is_refused_without_panicking() {
        // The session should never submit one, but a panic here would
        // drop a client's connection over a blank line.
        assert_eq!(
            handle(&snapshot_stopped(), 0, &[], MAX_CHUNK),
            Outcome::Reject("ACK [5@0] {} unsupported".to_string())
        );
    }

    // ------------------------------------------------------------------
    // Covers
    // ------------------------------------------------------------------

    /// The `href` published by the state frame, the one the cover frame
    /// must carry too.
    const HREF: &str = "/api/cover/1a2b3c";

    /// The URI our `currentsong` publishes for the snapshot below: the
    /// radio playing its second preset.
    const CURRENT_URI: &str = "ritornello://radio/2";

    /// A size that is **not** a multiple of `MAX_CHUNK`: three chunks, the
    /// last one shorter. A round size would let an implementation that
    /// always returns `MAX_CHUNK` bytes through.
    const SAMPLE_SIZE: usize = MAX_CHUNK * 2 + 1234;

    /// A snapshot where a cover has arrived, **consistent with the state**.
    ///
    /// This is the only shape the producer can emit, and that is the
    /// point: the core sends the state frame (which carries `cover_href`)
    /// *then* the bytes under the same `href`. A snapshot where the cover
    /// and the state would not agree also exists — it is the window
    /// between the two frames — but that is a different case, tested
    /// separately.
    fn snapshot_with_cover(size: usize) -> Snapshot {
        let mut inst = snapshot_playing();
        inst.state.track.cover_href = Some(HREF.to_string());
        inst.state.track.cover_origin = Some("files".to_string());
        inst.cover = Some(crate::state::test_cover(HREF, size).into());
        inst
    }

    /// The binary payload of a response, or a panic naming what was
    /// received instead.
    fn bytes_of(inst: &Snapshot, words: &[&str]) -> Binary {
        match handle_words(inst, 0, words) {
            Outcome::Bytes(b) => b,
            other => panic!("expected Bytes for {words:?}, got {other:?}"),
        }
    }

    #[test]
    fn albumart_announces_the_total_size_and_returns_the_first_chunk() {
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        let b = bytes_of(&inst, &["albumart", CURRENT_URI, "0"]);
        // `size:` is the size of the **whole image**, not of the chunk: it
        // is what tells the client how many round trips it has left.
        assert_eq!(b.header, vec![format!("size: {SAMPLE_SIZE}")]);
        assert_eq!(b.chunk, 0..MAX_CHUNK);
        assert_eq!(b.image.len(), SAMPLE_SIZE);
    }

    #[test]
    fn readpicture_adds_the_mime_type_and_serves_the_same_bytes() {
        // The two names, a single image: this device has only one cover
        // per track, whatever its origin. M.A.L.P. tries one then the
        // other, so both must succeed — and at the same spot.
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        let art = bytes_of(&inst, &["albumart", CURRENT_URI, "0"]);
        let pic = bytes_of(&inst, &["readpicture", CURRENT_URI, "0"]);
        assert_eq!(pic.header, vec![format!("size: {SAMPLE_SIZE}"), "type: image/jpeg".to_string()]);
        assert_eq!(pic.chunk, art.chunk);
        assert_eq!(pic.image, art.image);
    }

    #[test]
    fn the_chunks_follow_each_other_and_the_last_one_is_shorter() {
        // The chunking property seen from the pure module: the intervals
        // cover the image **exactly once**, with no hole and no overlap.
        // That is what makes a correct reassembly possible, and the
        // session test then verifies it on a real socket.
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        let mut expected = 0usize;
        let mut sizes = Vec::new();
        while expected < SAMPLE_SIZE {
            let b = bytes_of(&inst, &["albumart", CURRENT_URI, &expected.to_string()]);
            assert_eq!(b.chunk.start, expected, "the chunk must start at the requested offset");
            sizes.push(b.chunk.len());
            expected = b.chunk.end;
        }
        assert_eq!(expected, SAMPLE_SIZE, "the chunks must cover the whole image");
        assert_eq!(sizes, vec![MAX_CHUNK, MAX_CHUNK, 1234]);
    }

    #[test]
    fn an_offset_equal_to_the_size_returns_an_empty_chunk_and_not_a_refusal() {
        // MPD's own behavior, and the reason lies with the client: a loop
        // that closes with one request too many must not be refused what
        // it already has. The response is well-formed, simply empty.
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        let b = bytes_of(&inst, &["albumart", CURRENT_URI, &SAMPLE_SIZE.to_string()]);
        assert_eq!(b.header, vec![format!("size: {SAMPLE_SIZE}")]);
        assert!(b.chunk.is_empty(), "{:?}", b.chunk);
    }

    #[test]
    fn an_offset_beyond_the_size_is_an_argument_defect() {
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        let too_large = (SAMPLE_SIZE + 1).to_string();
        for name in ["albumart", "readpicture"] {
            assert_eq!(
                handle_words(&inst, 4, &[name, CURRENT_URI, &too_large]),
                Outcome::Reject(format!("ACK [2@4] {{{name}}} Offset too large")),
                "{name} should refuse an offset outside the image"
            );
        }
    }

    #[test]
    fn without_a_cover_both_commands_refuse_the_same_way() {
        // The **ordinary** case and not the exception: most streams have
        // no image at all. An `ACK 50` is what MPD answers when there is
        // no art, and that is what makes a client fall back to the other
        // name rather than getting stuck — an empty response crowned with
        // success would make a client that only tries `readpicture`
        // conclude "no image".
        let inst = snapshot_playing();
        assert!(inst.cover.is_none(), "the base fixture has no cover");
        for name in ["albumart", "readpicture"] {
            assert_eq!(
                handle_words(&inst, 0, &[name, CURRENT_URI, "0"]),
                Outcome::Reject(format!("ACK [50@0] {{{name}}} No file exists"))
            );
        }
    }

    #[test]
    fn a_uri_that_is_not_what_is_playing_is_refused() {
        // This arm's design decision. Serving the current image under a
        // stale URI would durably poison a client's cache, which files
        // covers **by URI**: station 3 would show station 2's image until
        // its next restart. The rejection, for its part, repairs itself at
        // the next wakeup.
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        for requested in [
            // Another preset of the same source.
            "ritornello://radio/3",
            // The same preset of another source.
            "ritornello://cd/2",
            // What a client talking to a real MPD would ask for.
            "Music/album/track.flac",
            "",
        ] {
            assert_eq!(
                handle_words(&inst, 0, &["albumart", requested, "0"]),
                Outcome::Reject("ACK [50@0] {albumart} No file exists".to_string()),
                "{requested} wrongly served"
            );
        }
        // And the current URI, for its part, is indeed served: without
        // this half, the test would pass with an implementation that
        // refuses everything.
        assert!(matches!(
            handle_words(&inst, 0, &["albumart", CURRENT_URI, "0"]),
            Outcome::Bytes(_)
        ));
    }

    #[test]
    fn a_cover_that_no_longer_describes_the_current_state_is_refused() {
        // **The window between the two frames.** The core sends the state
        // first and the cover next: there is therefore an instant where
        // the state designates the next track while the held cover is
        // still the previous one's. Without this check, `albumart` would
        // serve the old image **under the new URI** — precisely the case
        // that poisons the client's cache, reached without anyone having
        // done anything wrong.
        let mut inst = snapshot_with_cover(SAMPLE_SIZE);
        inst.state.track.cover_href = Some("/api/cover/next".to_string());

        assert_eq!(
            handle_words(&inst, 0, &["albumart", CURRENT_URI, "0"]),
            Outcome::Reject("ACK [50@0] {albumart} No file exists".to_string())
        );
    }

    #[test]
    fn without_a_current_preset_no_uri_designates_anything() {
        // `currentsong` publishes no `file:` in this state, so no client
        // can have a legitimate URI to ask for.
        let mut inst = snapshot_with_cover(SAMPLE_SIZE);
        inst.state.preset = None;
        assert_eq!(
            handle_words(&inst, 0, &["albumart", CURRENT_URI, "0"]),
            Outcome::Reject("ACK [50@0] {albumart} No file exists".to_string())
        );
    }

    #[test]
    fn both_commands_require_a_uri_and_an_offset() {
        let inst = snapshot_with_cover(SAMPLE_SIZE);
        for name in ["albumart", "readpicture"] {
            for words in [vec![name], vec![name, CURRENT_URI], vec![name, CURRENT_URI, "0", "0"]] {
                assert_eq!(
                    handle_words(&inst, 1, &words),
                    Outcome::Reject(format!("ACK [2@1] {{{name}}} wrong number of arguments")),
                    "{words:?} wrongly accepted"
                );
            }
            // A non-numeric offset is a different defect, and it is named
            // differently: the client will know which of its two
            // arguments to revisit.
            for offset in ["abc", "-1", "1.5", ""] {
                assert_eq!(
                    handle_words(&inst, 1, &[name, CURRENT_URI, offset]),
                    Outcome::Reject(format!("ACK [2@1] {{{name}}} integer expected")),
                    "offset {offset:?} wrongly accepted"
                );
            }
        }
    }

    #[test]
    fn both_names_are_announced_by_commands() {
        // The two halves of `commands`'s honesty, on these two precise
        // names: they are in the list, and the list is what the response
        // publishes. `every_announced_command_is_really_handled` closes
        // the pair by checking that neither of them falls into the
        // default rejection.
        let lines = handle_ok(&snapshot_with_cover(SAMPLE_SIZE), &["commands"]);
        for name in ["albumart", "readpicture"] {
            assert!(COMMANDS.contains(&name), "{name} missing from COMMANDS");
            assert!(lines.contains(&format!("command: {name}")), "{name} not announced");
        }
    }
}
