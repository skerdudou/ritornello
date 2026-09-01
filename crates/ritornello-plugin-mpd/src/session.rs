//! The dialogue with a client: the only part of the plugin that touches a
//! socket.
//!
//! One task per connection, and that is the entire architecture: every question
//! is answered from the shared state (one read-lock acquisition), every action
//! is a send on a bounded channel. No session ever has to wait for the core,
//! so **none can hold another back** — a client asleep in an `idle` costs
//! only one waiting task.
//!
//! Command lists and `idle` live here and not in `commands.rs`, because they
//! are facts about the **connection** and not about a command:
//! `command_list_begin` does nothing but change what the following lines mean,
//! and `idle` does nothing but suspend the reading of lines. `commands.rs`
//! stays pure, and is tested without a socket.

use crate::commands::{
    cover_announced_but_missing, handle, Binary, Outcome, MAX_CHUNK,
    MAX_CHUNK_CAP,
};
use crate::state::{SharedState, Snapshot, Subsystem};
use crate::protocol::{ack, split, line, Ack};
use anyhow::Result;
use ritornello_proto::InputMessage;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};

/// The version announced in the banner.
///
/// **It is a lie, and that must be said**: this plugin does not implement all
/// of MPD 0.23.5, it implements what `commands` enumerates. The lie is
/// deliberate because clients derive their capabilities from this number and
/// not from `commands` alone — libmpdclient and M.A.L.P. compare the announced
/// version before emitting `plchanges`, `seekcur` or `tagtypes` — so announcing
/// a low version would make them give up commands we actually handle.
/// The opposite risk (announcing too high) is bounded by `commands`, which
/// tells the truth, and by the `ACK 5`s of the rest.
const ANNOUNCED_VERSION: &str = "0.23.5";

/// Cap on **simultaneous sessions**.
///
/// The multiplier of everything that follows: each cap below bounds one
/// connection, and nothing bounded the number of connections. Yet the real
/// residue of a session can reach some ten mebibytes (see `MAX_RESPONSE` for
/// the math), so a hundred sessions add up to the device's gigabyte — the
/// failure all these caps exist to avoid, reached through the only path they
/// left open.
///
/// **16, justified by the real population**: a phone, a second phone, `mpc` on
/// the device, at a stretch a tablet and a desktop client — five at the very
/// most, and MPD clients open only one connection each (sometimes a second
/// one, to hold an `idle` apart). 16 therefore leaves three times the margin
/// of any legitimate use, while bounding the worst case to a bit under 200 MiB
/// where it used to be unbounded.
///
/// **And it is not a protection against malice alone**: a client that leaks
/// its connections — that reopens one on every network recovery without
/// closing the previous one — gets there by accident, and that is even the
/// more likely of the two cases. The refusal is then what keeps the device
/// alive while that client misbehaves, and the log names the cap so the cause
/// can be read without guessing.
///
/// A cap and not a waiting queue: making a connection wait behind a reached
/// cap would keep a descriptor open and let the client believe it is being
/// served. Refusing right away tells it the truth, and it is an answer a
/// client knows how to interpret — an unreachable MPD server is a state all of
/// them know how to display.
///
/// **The 200 MiB above only count the text path, and it must be said here**:
/// since covers arrived, this cap also multiplies `COVER_MAX_BYTES`. A session
/// answering `albumart` holds on to the image generation it serves during its
/// whole `write_all`, so sixteen motionless clients pin sixteen generations —
/// 16 × 20 MiB = **320 MiB**, plus the one the state holds itself, i.e.
/// **340 MiB**. The full math and what is not mitigated are on
/// `commands::MAX_CHUNK`; what to remember here is that this cap is the only
/// factor bounding that product.
const MAX_SESSIONS: usize = 16;

/// Cap on commands accumulated in a list, before `command_list_end`.
///
/// This is not decorative caution: between `command_list_begin` and its `end`,
/// the session **memorizes** every line without executing anything, so a
/// client (or a chatty port scanner) that never sends the `end` grows a `Vec`
/// without bound in a process running on a Pi. MPD has the same bound,
/// expressed in bytes (`max_command_list_size`, 2 MiB by default); here it is
/// a number of commands, simpler to justify and sufficient for the same
/// effect. 2048 is far beyond what a real client sends — M.A.L.P. groups about
/// ten commands.
const MAX_LIST_COMMANDS: usize = 2048;

/// Cap on the **bytes** accumulated by a command list.
///
/// The command count is not enough: an accumulated line can legitimately weigh
/// up to `MAX_LINE`, so 2048 commands bound the memory to 16 MiB per
/// connection — the very order of magnitude `MAX_LINE` exists to forbid. It
/// is, by the way, in bytes and not in commands that MPD expresses its own cap
/// (`max_command_list_size`, 2 MiB by default).
///
/// 256 KiB, i.e. 2048 commands of 128 bytes on average: a `setvol 30` weighs
/// ten, and the longest realistic command — a quoted name — weighs a few
/// hundred. Far above what a client sends.
///
/// **This cap counts text bytes, not heap bytes**, and the gap is not
/// cosmetic: what is accumulated is a `Vec<Vec<String>>`, and `split`
/// allocates one string **per token**. A legal 8 KiB line made of `"a a a a …"`
/// thus becomes ~4096 one-character `String`s, each costing its 24 bytes of
/// structure in the `Vec` plus an allocation the allocator rounds up — on the
/// order of 50 bytes per useful character, a factor close to thirty. 256 KiB
/// as counted can therefore weigh several real mebibytes. The cap is a cap
/// nonetheless; it is its unit that must not be mistaken for memory, and
/// `MAX_SESSIONS` is what bounds the product.
const MAX_LIST_BYTES: usize = 256 * 1024;

/// Cap on the **bytes** of a response, before writing.
///
/// It is the same leak as `MAX_LINE` taken from the other end, and the command
/// cap of a list does not bound it at all: it bounds the commands, not what
/// they **produce**. A list of 2048 `playlistinfo` — 26 KiB of input, one
/// loop, no malice — yields four lines per queue entry, i.e. up to 1020 lines
/// per command at maximal `preset_count` (255): two million `String`s on one
/// side, and above all **a contiguous allocation of several tens of
/// mebibytes** at the moment everything is flattened for the `write_all`. On a
/// Pi 2 B, a contiguous request of that size fails against fragmented memory
/// well before the total is reached.
///
/// 1 MiB: the longest legitimate response is a full `playlistinfo` — 255
/// entries of four lines, some fifteen kibibytes in all, `preset_count` being
/// an `Option<u8>` — so the cap lets about sixty of them through in a single
/// list.
///
/// **What this cap bounds, and what it does not.** It is checked after each
/// command of the batch and not on each line, so an overrun is detected at
/// most one command response late (some fifteen kibibytes), and the
/// `Outcome::Cancel` arm pushes its `list_OK` without checking it at all —
/// bounded by the command count, so 2048 × 8 bytes, i.e. 16 KiB. The residue
/// past the cap is thus some thirty kibibytes, and not "one command response".
///
/// **Two multipliers to know when recomputing what a session really costs** —
/// stating them is better than writing down a number the next change will
/// contradict:
///
/// 1. **The simultaneous copy.** `write` flattens the response into a `String`
///    whose exact capacity it reserves *while* `Response.lines` is still
///    alive: the text therefore exists twice at that instant. The **counted**
///    peak of a session is thus ≈ 2 × 1 MiB (the response and its copy) +
///    256 KiB (the accumulated list) ≈ 2.3 MiB, and not 1.3.
/// 2. **Text bytes versus heap bytes.** As with `MAX_LIST_BYTES`, these caps
///    count text while the structures hold `String`s: a one-mebibyte response
///    in lines of some twenty bytes is ~40,000 `String`s, i.e. double that on
///    the heap. End to end, a session pushed to both of its caps holds on the
///    order of **6 to 12 real MiB**.
///
/// The lever that matters, should that figure become a problem, is
/// `MAX_LIST_BYTES` (the dominant term, because of the factor of thirty on
/// one-character tokens), and not `MAX_SESSIONS` — but `MAX_SESSIONS` is what
/// bounds the product.
const MAX_RESPONSE: usize = 1024 * 1024;

/// Cap on a command **line**, in bytes.
///
/// Without it, this is the last unbounded surface of a port open to the whole
/// local network: a client that connects and sends bytes **without ever
/// sending a newline** makes the plugin allocate until the allocator gives up.
/// On this device — a Pi 2 B, one gigabyte shared between mpv, the core, the
/// web UI and eight plugins — that does not only take the plugin down, it
/// takes the music down. And it requires no malice: a port scanner or a buggy
/// client does it by accident, and the port is reachable from the whole local
/// network without a password.
///
/// 8 KiB is twice MPD's own input buffer (4 KiB) and an order of magnitude
/// above the longest legitimate line of the protocol — a quoted playlist name
/// inside a command list, a few hundred bytes at worst. Far above the real,
/// far below what costs: one line buffer per session, so 128 KiB for the
/// `MAX_SESSIONS` allowed.
///
/// (This doc carried for a while the sentence "even a hundred simultaneous
/// connections thus reserve only one megabyte". It was true when this buffer
/// was the whole story, and it became false by a factor of a thousand as soon
/// as the accumulated list and the composed response got their own caps: the
/// line buffer is now only a minor term of a session's residue. See
/// `MAX_RESPONSE` for the full math.)
const MAX_LINE: usize = 8 * 1024;

/// The session's line reader: a `BufReader`, plus the cap.
///
/// Hand-written (`fill_buf`/`consume`) rather than with `BufReader::lines()`,
/// for the only reason that matters: `lines()` accumulates up to the `\n`
/// **without bound**. See `MAX_LINE`.
struct BoundedReader {
    playback: BufReader<OwnedReadHalf>,
    /// The line read during an `idle` wait and **pushed back in queue** for
    /// the `serve` loop.
    ///
    /// This is the mechanism of the "implicit `noidle`": a command received
    /// during an `idle` cancels the wait (bare `OK`) *then* must be executed
    /// like any other line — hence passed again through the full dispatch of
    /// `serve`, command lists and unreadable lines included, rather than
    /// half-reinterpreted in `wait_idle`.
    ///
    /// A single line is enough, and a single place says so: it is pushed back
    /// right after being read, and consumed on the next loop turn, so two
    /// cannot coexist.
    pushback: Option<String>,
    /// The bytes of the current line, between two `\n`.
    ///
    /// It lives in the struct and not on `next_line`'s stack, and that is no
    /// detail: it is what makes that function **cancel-safe**, exactly like
    /// the buffer of `tokio::io::Lines`. `wait_idle` puts it in a `select!`
    /// with the wakeup, so it is abandoned midway every time a sleeper wakes
    /// up — if the buffer were local, the half line already read would leave
    /// with it, and the next command would be truncated.
    buffer: Vec<u8>,
}

impl BoundedReader {
    fn new(playback: OwnedReadHalf) -> Self {
        Self { playback: BufReader::new(playback), pushback: None, buffer: Vec::new() }
    }

    /// Puts an already-read line back in front of the stream. See `pushback`.
    fn put_back(&mut self, line: String) {
        debug_assert!(self.pushback.is_none(), "two lines pushed back in queue at once");
        self.pushback = Some(line);
    }

    /// The next line without its `\n`, or `None` at end of stream.
    ///
    /// A line exceeding `MAX_LINE` is an **error**, hence the end of the
    /// session: that is what MPD does, and the only defensible choice here.
    /// An `ACK` would require naming the offending command — impossible, the
    /// line is truncated — then discarding an unknown number of bytes up to
    /// the next `\n`, that is, keeping a connection that has already left the
    /// protocol. Closing is immediate, well-defined, and logged by
    /// `accept_loop`.
    async fn next_line(&mut self) -> Result<Option<String>> {
        // The pushed-back line goes before the socket, and **without an await
        // point**: this `take` and this `return` are in the same poll, so a
        // cancellation cannot slip between the two and lose the line.
        if let Some(line) = self.pushback.take() {
            return Ok(Some(line));
        }
        loop {
            let available = self.playback.fill_buf().await?;
            if available.is_empty() {
                // End of stream. A last line without `\n` is returned anyway,
                // as `Lines` did: a client that closes its write half right
                // after a command must see that command processed.
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.buffer);
                return Ok(Some(Self::finish(line)?));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(end) => {
                    // The cap is checked **before** copying: an overrun must
                    // not first allocate what it refuses. Checked in both
                    // arms, and not only in the one without `\n`, so the bound
                    // holds whatever the capacity of the `BufReader`.
                    if self.buffer.len() + end > MAX_LINE {
                        anyhow::bail!("command line longer than {MAX_LINE} bytes");
                    }
                    self.buffer.extend_from_slice(&available[..end]);
                    self.playback.consume(end + 1);
                    let line = std::mem::take(&mut self.buffer);
                    return Ok(Some(Self::finish(line)?));
                }
                None => {
                    let received = available.len();
                    if self.buffer.len() + received > MAX_LINE {
                        anyhow::bail!(
                            "command line longer than {MAX_LINE} bytes without a newline"
                        );
                    }
                    self.buffer.extend_from_slice(available);
                    self.playback.consume(received);
                }
            }
        }
    }

    /// The bytes of a line as a `String`.
    ///
    /// A trailing `\r` is removed: `\r\n` is what clients written on Windows
    /// send, and without this `ping\r` would be an unknown command. It is also
    /// what `Lines` did — losing it while changing readers would have been a
    /// regression no existing test saw.
    ///
    /// A non-UTF-8 byte is an error, hence the end of the session: there too
    /// the behavior of `Lines`, kept as is. The MPD protocol is textual, and a
    /// command whose bytes do not form text cannot be split.
    fn finish(mut line: Vec<u8>) -> Result<String> {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Ok(String::from_utf8(line)?)
    }
}

/// The lines of a response being composed, and their weight in bytes.
///
/// The count is kept as it goes rather than recomputed: the cap check happens
/// after each command of a batch, and re-summing the whole response every time
/// would make composing a long list quadratic. It counts the `\n` of each
/// line, so it is exactly the number of bytes `write` will put on the socket.
#[derive(Default)]
struct Response {
    lines: Vec<String>,
    bytes: usize,
}

impl Response {
    fn push(&mut self, line: String) {
        self.bytes += line.len() + 1;
        self.lines.push(line);
    }

    fn extend(&mut self, lines: Vec<String>) {
        for line in lines {
            self.push(line);
        }
    }

    /// True when the response exceeds `MAX_RESPONSE`.
    fn too_large(&self) -> bool {
        self.bytes > MAX_RESPONSE
    }
}

/// What belongs to only **one** connection, and thus travels with it.
///
/// Grouped because they have exactly the same nature — two facts about the
/// client, that nothing shares between sessions — and not to shorten a
/// signature: separating them would suggest they have different lifetimes.
/// The command-list state resembles them but stays in `serve`: it never
/// crosses a call to `execute`, since `serve` is what decides what a batch is.
struct Connection {
    /// The subsystem counters this connection has already seen: the reference
    /// of all its `idle`s. Read by `execute`, advanced only by `wait_idle`,
    /// and only for the subsystems a wakeup announces.
    seen: [u64; 4],
    /// The chunk size this client accepts for binary responses (see
    /// `commands::binarylimit`). `MAX_CHUNK` as long as it has asked for
    /// nothing — the protocol default.
    binary_limit: usize,
}

/// What the session must do after a batch of commands.
enum Next {
    /// Keep reading lines.
    Continue,
    /// Close the connection: `close`, or a dead `input` half.
    Close,
}

/// Accepts connections, each in its own task, and **rebinds when the settings
/// change**.
///
/// The admin page said "the change only takes effect when the plugin
/// restarts", and it was true: the socket was bound once and for all in
/// `main`. That is no longer the case — a successful save pushes the new
/// configuration on `config_rx`, and this loop binds the new address/port
/// pair.
///
/// **Three decisions, each for a reason:**
///
/// - **The old listener is only released once the new one is bound.** If the
///   requested port is already taken, or the address absent from the machine,
///   the device keeps serving where it served: a faulty setting must not make
///   the MPD server unreachable, while the very page that caused it is still
///   open. The failure goes to the log, and the page will say the opposite —
///   the file, for its part, was indeed saved. That is the accepted trade-off:
///   port validation cannot anticipate that it is occupied.
/// - **Already-open sessions are not cut.** They hold their own `TcpStream`,
///   which closing the listener does not touch. A phone in the middle of
///   listening therefore keeps its connection until it closes it itself,
///   where a real MPD restart would have torn it away.
/// - **The session cap survives rebinds.** The semaphore lives here, outside
///   the loop: recreating it on every settings change would make
///   `MAX_SESSIONS` circumventable by a mere repeated save.
///
/// `accept` is cancellable without loss (that is tokio's guarantee), so losing
/// the `select!` race never drops an already-accepted connection.
pub async fn listen(
    listener: TcpListener,
    mut config_rx: tokio::sync::watch::Receiver<crate::config::Config>,
    state: Arc<SharedState>,
    cmd_tx: mpsc::Sender<InputMessage>,
) {
    let slots = Arc::new(Semaphore::new(MAX_SESSIONS));
    let mut listener = listener;
    loop {
        tokio::select! {
            // Never yields: its only exit is being cancelled by the other
            // arm.
            () = accept_loop(&listener, &slots, &state, &cmd_tx) => {}
            change = config_rx.changed() => {
                if change.is_err() {
                    // The admin half is gone (the plugin is shutting down): no
                    // rebind will ever come, but there is still serving to do.
                    tracing::debug!("mpd settings channel closed; keeping the current socket");
                    accept_loop(&listener, &slots, &state, &cmd_tx).await;
                    return;
                }
                let c = config_rx.borrow_and_update().clone();
                match TcpListener::bind((c.listen.as_str(), c.port)).await {
                    Ok(new_listener) => {
                        tracing::info!("mpd server now listening on {}:{}", c.listen, c.port);
                        listener = new_listener;
                    }
                    Err(e) => tracing::warn!(
                        "mpd could not listen on {}:{} ({e}); keeping the previous socket",
                        c.listen,
                        c.port
                    ),
                }
            }
        }
    }
}

/// The accept loop itself. Never yields.
///
/// An `accept` error is logged and the loop continues: an exhausted descriptor
/// or a connection reset before the `accept` must not take the server down,
/// otherwise the port stays open in a process that no longer listens.
///
/// The slot semaphore is **passed in** and not created here: it lives in
/// `listen`, so that the session cap survives rebinds (see its doc). One slot
/// per session, returned no matter what — the permit lives in the task, so it
/// leaves with it, including if it panics, since its `Drop` is what returns
/// it. A `Semaphore` rather than an atomic counter for exactly this reason: a
/// counter would require remembering to decrement it on every exit path, and
/// the day one is forgotten the device would refuse everyone after sixteen
/// connections.
async fn accept_loop(
    listener: &TcpListener,
    slots: &Arc<Semaphore>,
    state: &Arc<SharedState>,
    cmd_tx: &mpsc::Sender<InputMessage>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, address)) => {
                // `try_acquire_owned` and not `acquire`: past the cap the
                // connection is **refused**, not queued. See `MAX_SESSIONS`.
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    tracing::warn!("mpd refusing {address}: {MAX_SESSIONS} sessions already open");
                    drop(stream);
                    continue;
                };
                tracing::info!("mpd client connected from {address}");
                let state = state.clone();
                let cmd_tx = cmd_tx.clone();
                // One task per client, detached: this is what makes a session
                // unable to hold another back. The `spawn` returns nothing to
                // watch — a session that ends has nothing more to say than
                // what it logs here.
                tokio::spawn(async move {
                    match serve(stream, state, cmd_tx).await {
                        Ok(()) => tracing::info!("mpd client {address} disconnected"),
                        Err(e) => tracing::info!("mpd session with {address} ended: {e}"),
                    }
                    // Explicit, though the scope would take care of it: this
                    // is the line that says a slot is freed here, and hunting
                    // for it in a closing brace would be a guessing game.
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::warn!("mpd accept failed: {e}");
            }
        }
    }
}

/// The dialogue of one connection, from the first byte written to the last
/// one read.
///
/// The list state lives in this function and nowhere else: it only makes
/// sense for this connection, and two clients, one of which is in the middle
/// of a list, do not see each other.
pub async fn serve(stream: TcpStream, state: Arc<SharedState>, cmd_tx: mpsc::Sender<InputMessage>) -> Result<()> {
    let (playback, mut writer) = stream.into_split();
    let mut lines = BoundedReader::new(playback);

    // The banner leaves without anything being asked: that is the protocol,
    // and a client waits for this line before writing anything.
    writer.write_all(format!("OK MPD {ANNOUNCED_VERSION}\n").as_bytes()).await?;

    // **The counters this connection has already seen, read once and for
    // all.** This is the reference of all its `idle`s, and it lives here
    // because it is a fact about the **connection** — like the list state
    // just below, and for the same reason.
    //
    // Reading them at the banner and carrying them, rather than re-reading
    // them in the `Snapshot` of each `idle` command, fixes a real defect: the
    // per-command read swallowed everything that had moved between the
    // previous response and the `idle` line, that is, during the only window
    // where an MPD client is not listening. The real MPD accumulates its
    // flags per connection since the connection began; for `stored_playlist`,
    // swallowing an event leaves `listplaylists` stale until the next sources
    // catalog change — hence potentially forever. See `state::versions`.
    //
    // An `idle` woken immediately on a change this client had maybe already
    // read through a `status` is the acceptable direction of error: a
    // spurious wakeup costs it a redundant query, a missed wakeup costs it
    // the correctness of its screen.
    let mut connection = Connection { seen: state.versions().await, binary_limit: MAX_CHUNK };

    // The accumulated commands of an in-progress list, `None` outside a list.
    // An `Option<Vec<_>>` rather than a `Vec` plus a boolean: "not in a list"
    // and "in a still-empty list" are two different states, and a
    // `command_list_end` received outside a list must be refused as an
    // unknown command rather than return a complacent `OK`.
    let mut list: Option<Vec<Vec<String>>> = None;
    let mut with_ok = false;
    // The bytes already accumulated by the current list. Reset to zero at
    // each opening, like `with_ok`.
    let mut list_bytes = 0usize;

    while let Some(raw) = lines.next_line().await? {
        let args = match split(&raw) {
            Ok(args) => args,
            Err(code) => {
                // An unreadable line is an `ACK`, never a break: a client
                // that misquoted a station name must be able to continue
                // without reconnecting.
                //
                // An in-progress list is abandoned though: a command is
                // missing from it, so executing it later would execute a
                // batch that is not the one the client wrote.
                let index = list.as_ref().map_or(0, Vec::len);
                list = None;
                let rejection = ack(code, index, first_word(&raw), "invalid argument");
                write(&mut writer, &[rejection]).await?;
                continue;
            }
        };
        // `""` for an empty line: `handle` already refuses it (it is total by
        // construction), so nothing here needs a special case.
        let word = args.first().map_or("", String::as_str);

        if list.is_some() {
            match word {
                "command_list_end" => {
                    let batch = list.take().unwrap_or_default();
                    match execute(&mut lines, &mut writer, &state, &cmd_tx, &batch, with_ok, &mut connection)
                        .await?
                    {
                        Next::Continue => {}
                        Next::Close => break,
                    }
                }
                // `idle` in a list: MPD forbids it, and for a good reason —
                // accepting it would require suspending a half-written list,
                // whose final `OK` cannot leave before the wakeup. The index
                // carried is the **rank** `idle` occupies in the list,
                // otherwise the client does not know which of its commands
                // was refused.
                //
                // Refused **at accumulation** and not at execution: the list
                // can never be executed, so first executing the commands that
                // precede it would emit real actions (a `next`, a `setvol`)
                // on behalf of a batch the client will never see complete.
                "idle" => {
                    let index = list.as_ref().map_or(0, Vec::len);
                    list = None;
                    let rejection = ack(Ack::Unknown, index, "idle", "not allowed in command list");
                    write(&mut writer, &[rejection]).await?;
                }
                // **A binary response in a command list: MPD allows it, this
                // plugin refuses it.** Three reasons, in order of weight:
                //
                // 1. It would break this session's write discipline.
                //    `execute` composes a *whole* batch as text, checks the
                //    cap, then writes **once** — which guarantees a
                //    half-written response is never read as complete.
                //    Inserting bytes in the middle would force either
                //    flushing the accumulator before each image (thus giving
                //    up that guarantee), or passing the bytes through the
                //    text accumulator — impossible, they are not UTF-8.
                // 2. It would reopen the amplifier Task 8 closed: 2048
                //    `albumart` in a list is 26 KiB of input for 16 MiB
                //    written, **accumulated before the first write**. That is
                //    exactly the measurement that gave birth to
                //    `MAX_RESPONSE`, on this same unauthenticated port.
                // 3. Nobody needs it. A cover is fetched through a sequence
                //    of round trips where each offset depends on the `size:`
                //    the previous one returned — yet a command list is sent
                //    **entirely before** being read. The client therefore
                //    cannot compose the batch it would take.
                //
                // Refused at accumulation, like `idle` and for the same
                // reason: the batch can never complete, so first executing
                // the commands preceding it would emit real actions on behalf
                // of a batch the client will never see.
                "albumart" | "readpicture" => {
                    let index = list.as_ref().map_or(0, Vec::len);
                    list = None;
                    let rejection = ack(Ack::Unknown, index, word, "not allowed in command list");
                    write(&mut writer, &[rejection]).await?;
                }
                _ => {
                    let index = list.as_ref().map_or(0, Vec::len);
                    // Two bounds for a single refusal: the command count
                    // (which bounds a batch's work) and their weight in bytes
                    // (which bounds memory, an accumulated line possibly
                    // weighing up to `MAX_LINE`). See both constants.
                    list_bytes += raw.len() + 1;
                    if index >= MAX_LIST_COMMANDS || list_bytes > MAX_LIST_BYTES {
                        list = None;
                        let rejection = ack(Ack::Unknown, index, word, "list too large");
                        write(&mut writer, &[rejection]).await?;
                    } else if let Some(accumulated) = list.as_mut() {
                        // Accumulated without being looked at: a nested
                        // `command_list_begin`, an unknown word or an empty
                        // line will be refused by `handle` at execution, at
                        // their rank, and will interrupt what follows like
                        // any other error. No special case to write here.
                        accumulated.push(args);
                    }
                }
            }
            continue;
        }

        match word {
            "command_list_begin" => {
                list = Some(Vec::new());
                with_ok = false;
                list_bytes = 0;
            }
            "command_list_ok_begin" => {
                list = Some(Vec::new());
                with_ok = true;
                list_bytes = 0;
            }
            _ => {
                let batch = std::slice::from_ref(&args);
                match execute(&mut lines, &mut writer, &state, &cmd_tx, batch, false, &mut connection)
                    .await?
                {
                    Next::Continue => {}
                    Next::Close => break,
                }
            }
        }
    }
    Ok(())
}

/// Executes a batch — a single command, or the commands of a list — and
/// **writes the response itself**.
///
/// A single path for both cases: a command outside a list is a one-command
/// batch with `with_ok` false. This is what guarantees a list answers exactly
/// like the sequence of commands it contains, up to `list_OK`.
///
/// `lines` is only there for `idle`: it is the only outcome that needs to
/// keep reading (the `noidle` that cancels it, or the command that replaces
/// it) before having answered.
///
/// `connection` carries what belongs only to this client: the counter
/// reference of its `idle`s and the chunk size it accepts (see `Connection`).
async fn execute(
    lines: &mut BoundedReader,
    writer: &mut OwnedWriteHalf,
    state: &SharedState,
    cmd_tx: &mpsc::Sender<InputMessage>,
    batch: &[Vec<String>],
    with_ok: bool,
    connection: &mut Connection,
) -> Result<Next> {
    let Connection { seen, binary_limit } = connection;
    let mut output = Response::default();
    for (index, args) in batch.iter().enumerate() {
        // **A single snapshot, read before `handle`.** One lock acquisition
        // for everything the response publishes: reading in two steps would
        // let `status` contradict itself in its own middle.
        //
        // **Its counters do not serve as the reference of an `idle`, and that
        // is the point.** They describe the instant of *this* command; the
        // reference of an `idle` is the one the connection carries since its
        // banner. Confusing the two — which this code used to do — swallows
        // any change occurred between the previous response and the `idle`
        // line, and the comment that lived here claimed the opposite: nothing
        // in this read makes "the missed wakeup impossible". It is `wait`'s
        // comparison against the connection's reference that forbids it.
        let mut snapshot = state.read().await;
        // **The only wait this module allows itself before handling**, and it
        // fixes the cover that vanished on every track change: see
        // `cover_announced_but_missing` and `wait_cover`.
        if cover_announced_but_missing(&snapshot, args) {
            snapshot = wait_cover(state, snapshot).await;
        }
        match handle(&snapshot, index, args, *binary_limit) {
            Outcome::Reply { lines: reply_lines, cmds } => {
                for cmd in &cmds {
                    // **Push first, acknowledge second.** The channel can
                    // refuse (full, or dead `input` half) and acknowledging a
                    // switch we did not emit would be worse than not
                    // acknowledging it: `status` would lie until the next
                    // frame, and a woken `idle` would announce a change that
                    // did not happen.
                    //
                    // `held` false, never anything else: `held` means "key
                    // held down", a keyboard notion the network does not
                    // have.
                    let message = InputMessage { cmd: cmd.clone(), held: false };
                    if cmd_tx.send(message).await.is_err() {
                        // The `input` half is dead: nothing this client says
                        // can succeed any more, so letting it talk would be
                        // lying to it.
                        tracing::warn!("mpd input channel closed; closing session");
                        return Ok(Next::Close);
                    }
                }
                state.acknowledge_optimistic(&cmds).await;
                output.extend(reply_lines);
                if with_ok {
                    output.push("list_OK".to_string());
                }
                // The response cap, checked here because this is the only
                // place the response grows. Nothing has been written yet, so
                // the rejection **replaces** everything composed so far: the
                // client receives exactly one terminator for its request, its
                // own accounting stays correct, and the connection survives —
                // unlike the too-long line, where closing was the only
                // defensible choice since we could not even name the
                // offending command. Here we do name it, and its rank along
                // with it.
                //
                // **What the rejection costs, and it must be said**:
                // commands `0..=index` have already pushed their
                // `InputMessage` and been optimistically acknowledged, so
                // their effects on the device **persist** even though their
                // output is discarded. A client that groups `setvol 40` then
                // a large `playlistinfo` will therefore see the volume
                // change without receiving a single line. This is exactly
                // the trade-off MPD makes on a mid-list error — commands
                // already executed stay executed — and it is acceptable for
                // the same reason: undoing what has already gone to the core
                // is not in our power, and the client can always re-read the
                // state.
                if output.too_large() {
                    tracing::warn!("mpd response over {MAX_RESPONSE} bytes; refusing");
                    let name = args.first().map_or("", String::as_str);
                    let rejection = ack(Ack::Unknown, index, name, "response too large");
                    write(writer, &[rejection]).await?;
                    return Ok(Next::Continue);
                }
            }
            // `noidle` received outside a wait: a bare `OK`, and inside a
            // list a `list_OK` like any other command without lines.
            //
            // Pushed **without checking the cap**, unlike the arm above:
            // eight bytes per command, so 16 KiB at worst for an entire batch
            // of `noidle`, which the command count already bounds. This is
            // what carries the residue past the cap to some thirty
            // kibibytes, and not to a single command's response.
            Outcome::Cancel => {
                if with_ok {
                    output.push("list_OK".to_string());
                }
            }
            // `binarylimit`: the value is already bounded by `commands`,
            // there is nothing left but to remember it. It holds for the
            // **rest** of this connection, including the commands that
            // follow in the same list — that is what MPD does, and it is the
            // only order that makes `binarylimit` followed by `albumart`
            // grouped together usable.
            Outcome::BinaryLimit(n) => {
                *binary_limit = n;
                if with_ok {
                    output.push("list_OK".to_string());
                }
            }
            // The first error produces its `ACK` and **nothing that follows
            // is executed**: the `for` stops there. The lines already
            // composed leave anyway, as MPD does — an `ACK` does not retract
            // the responses of the commands that, for their part, succeeded.
            Outcome::Reject(rejection) => {
                // **The rejection is logged with the whole command**, and
                // this is not a comfort. A client that hits an unhandled
                // command only displays a generic message — "unsupported" —
                // and the operator then has no way to know *which one*: this
                // is exactly what was missing to diagnose M.A.L.P.'s failure
                // selecting a track inside a saved playlist. The arguments
                // matter as much as the name: the same command can be
                // rejected for its shape.
                //
                // At `info` and not at `warn`: a rejection is an ordinary
                // protocol response (a client tries, learns, moves on), and
                // the core only keeps `warn`s for its "recent errors" panel —
                // pouring every unknown command from a chatty client into it
                // would fill it with noise.
                tracing::info!("mpd refused {args:?}: {rejection}");
                output.push(rejection);
                write(writer, &output.lines).await?;
                return Ok(Next::Continue);
            }
            // A binary response: it is written **alone**, through its own
            // path, and it closes the request — no `OK` appended by the rest
            // of the loop, `write_bytes` puts its own.
            //
            // `output` is necessarily empty here: the two binary commands
            // are rejected at list accumulation (see `serve`), so the batch
            // has only one command. Writing it anyway keeps this function
            // correct should that ever stop being the case, rather than
            // swallowing lines — the same choice as the `Wait` arm just
            // below, for the same reason.
            Outcome::Bytes(binary) => {
                write(writer, &output.lines).await?;
                write_bytes(writer, &binary).await?;
                return Ok(Next::Continue);
            }
            Outcome::Wait(subsystems) => {
                // `idle` never reaches this point inside a list: the list
                // rejected it at accumulation. Outside a list, the batch has
                // only one command, so `output` is empty — writing it anyway
                // keeps this function correct should a batch ever contain
                // several, rather than swallowing lines.
                write(writer, &output.lines).await?;
                return wait_idle(lines, writer, state, &subsystems, seen).await;
            }
            Outcome::Close => {
                // **`OK` then closing, and it is a choice.** MPD itself
                // writes nothing before closing on `close`. We answer, so
                // that this function's discipline has no exception: every
                // accepted command receives exactly one terminator. A client
                // that has already stopped reading simply makes this write
                // fail, which the session treats as an ordinary end — and a
                // client still reading finds its response where it expects
                // it. The divergence has no observable effect since the
                // connection closes in both cases; what matters is that it
                // be deliberate.
                output.push("OK".to_string());
                write(writer, &output.lines).await?;
                return Ok(Next::Close);
            }
        }
    }
    // A single `OK` closes the whole batch: this is what distinguishes a
    // command list from the same sequence of commands sent one by one.
    output.push("OK".to_string());
    write(writer, &output.lines).await?;
    Ok(Next::Continue)
}

/// How long a cover request waits for an image the device has already
/// announced.
///
/// Three seconds, and the number comes from the two deadlines it must cover:
/// the core bounds to `health::TIMEOUT` the playback of a cover file on a
/// share, and a network download is of the same order. Beyond that, the
/// image will probably not arrive for this track, and the rejection is the
/// right response.
///
/// What this wait does **not** put at risk: a session is a task on its own,
/// so waiting here holds nobody else back (see the module header). M.A.L.P.,
/// for that matter, opens a separate connection for images.
const COVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Waits, at most `COVER_TIMEOUT`, for the announced cover to arrive, and
/// returns the snapshot that will decide.
///
/// Returns control **as soon as** the wait no longer has an object: either
/// the image is there, or the state has changed enough that the request can
/// no longer be satisfied (next track, stop). It is
/// `cover_announced_but_missing` that decides — the same function that
/// decided to wait — a single statement of the condition, never two to keep
/// in sync.
///
/// At the deadline, returns the last snapshot read: `handle` will draw the
/// ordinary rejection from it, as if we had never waited.
async fn wait_cover(state: &SharedState, snapshot: Snapshot) -> Snapshot {
    let args = cover_arguments(&snapshot);
    let mut current = snapshot;
    let deadline = tokio::time::Instant::now() + COVER_TIMEOUT;
    loop {
        // `Subsystem::Player`: this is the subsystem `apply_cover` moves —
        // the MPD protocol has none for images, and the plugin chose this
        // one (see its doc). Waiting on it waits for exactly the image's
        // arrival, plus track changes, which must also wake us so we stop
        // waiting.
        let waiting = state.wait(&[Subsystem::Player], current.versions);
        if tokio::time::timeout_at(deadline, waiting).await.is_err() {
            tracing::debug!("mpd cover did not arrive within {COVER_TIMEOUT:?}");
            return current;
        }
        current = state.read().await;
        if !cover_announced_but_missing(&current, &args) {
            return current;
        }
    }
}

/// Rebuilds the arguments of a cover request for what is playing.
///
/// **Rebuilt and not carried along**, and the nuance is the point: the wait
/// loop must re-evaluate the condition against the *current* state, whereas
/// the URI the client wrote names the track of that moment. Rebuilding them
/// from the starting snapshot keeps exactly the question asked — "has this
/// track's image arrived?" — and makes the loop exit as soon as the track
/// changes, since the URI will no longer match.
fn cover_arguments(inst: &Snapshot) -> Vec<String> {
    vec![
        "albumart".to_string(),
        inst.state
            .preset
            .map(|p| crate::commands::uri(&inst.state.source, p))
            .unwrap_or_default(),
        "0".to_string(),
    ]
}

/// Holds an `idle` wait: returns control on wakeup, or on what the client
/// says in the meantime.
///
/// **An empty `subsystems` means waiting forever**, and not answering `OK`
/// right away (see `Outcome::Wait`'s doc): a client that named only
/// subsystems this plugin never emits (`idle database`) asked a legitimate
/// question whose answer is silence. It is `wait` that honors it without a
/// special case — no subsystem can ever defer, so it goes back to sleep at
/// every notification — and passing it the list as is is all there is to do.
/// Answering `OK` would make the client loop at full speed, exactly the
/// opposite of what `idle` exists to avoid.
///
/// **`seen` is the connection's reference, and this function is the only one
/// that advances it**: on an announced wakeup, and only for the announced
/// subsystems. Real MPD only clears the flags it just reported, and
/// advancing the whole array at once would lose the change of a subsystem a
/// following `idle` will ask for (`idle player` then `idle mixer` is the
/// shortest case of it). A `noidle`, for its part, announces nothing: it
/// therefore advances nothing, and the pending change will resurface at the
/// next `idle`.
async fn wait_idle(
    lines: &mut BoundedReader,
    writer: &mut OwnedWriteHalf,
    state: &SharedState,
    subsystems: &[Subsystem],
    seen: &mut [u64; 4],
) -> Result<Next> {
    // Two outcomes, and both must be listened to: the wakeup, and what the
    // client says during the wait — `noidle`, the only command MPD allows
    // there, or any other line, which then counts as an implicit `noidle`.
    // `BoundedReader::next_line` is cancel-safe (its buffer lives in the
    // struct, see there), so the losing branch loses no byte; and abandoning
    // `wait` loses no wakeup, since `seen` keeps the reference and the
    // counters are monotonic.
    let wakeup = tokio::select! {
        wakeup = state.wait(subsystems, *seen) => wakeup,
        read = lines.next_line() => {
            let Some(raw) = read? else {
                // The client left during its wait: nothing to write.
                return Ok(Next::Close);
            };
            // **A line received during the wait closes the `idle` with a
            // bare `OK`.** This is the protocol's accounting: an MPD client
            // counts one terminator per request, and it has written two —
            // its `idle`, then this line.
            //
            // This code used to reject this line with a single `ACK` and
            // write nothing for the `idle`: two requests shared one
            // terminator, and the client came out **permanently shifted by
            // one** — every following response read as the one for its
            // previous request. A silent, permanent shift, where MPD's
            // choice (closing) is loud and self-repairing. We keep the
            // choice not to close — "a faulty line is never a break", and a
            // reconnection would cost the client a defect no log shows it —
            // but by repairing what it had broken: the invariant this
            // function states about `Outcome::Close` becomes true again,
            // every accepted command receives exactly one terminator.
            //
            // **And the bare `OK` is not a form invented for the
            // occasion**: it is already what the `noidle` arm wrote, so it
            // is the same response in the same place. The fix only extends
            // an existing behavior to a second trigger — it cannot put on
            // the wire a form a client would never have seen.
            write(writer, &["OK".to_string()]).await?;
            // `noidle` is the only line that does not deserve a proper
            // response of its own: it is not a request but **the
            // cancellation of the one in progress**, and the `OK` just
            // written is as much its own as the `idle`'s — a single
            // terminator for `idle` + `noidle`, exactly like MPD. The rest
            // is an implicit `noidle` **followed by this command**, likely
            // what the client meant: the line therefore goes back into
            // `serve`'s full dispatch — command lists, unreadable lines and
            // `close` included — without a single case being reinterpreted
            // here.
            //
            // An unreadable line is not `noidle` (it does not split), and
            // that is the intended behavior: it will receive its `ACK` on
            // the next turn, like anywhere else.
            let is_noidle = split(&raw)
                .map(|args| args.first().is_some_and(|word| word == "noidle"))
                .unwrap_or(false);
            if !is_noidle {
                lines.put_back(raw);
            }
            // The connection's reference does not advance: nothing was
            // announced, so a change that occurred during this wait will
            // resurface at the next `idle`.
            return Ok(Next::Continue);
        }
    };
    // **The reported counters, and only those, are consumed.** Advancing the
    // whole table would lose the change of a subsystem this `idle` did not
    // ask for.
    for subsystem in &wakeup.moved {
        seen[*subsystem as usize] = wakeup.versions[*subsystem as usize];
    }
    let mut response: Vec<String> =
        wakeup.moved.iter().map(|subsystem| line("changed", subsystem_name(*subsystem))).collect();
    response.push("OK".to_string());
    write(writer, &response).await?;
    Ok(Next::Continue)
}

/// The MPD name of a subsystem, as a `changed:` publishes it.
///
/// This is the exact inverse of the table `commands.rs` uses to read an
/// `idle`: a name that diverged would announce a subsystem no client could
/// ask for again. A test verifies it by passing each of these names to
/// `idle` and requiring the same subsystem to come back out.
fn subsystem_name(subsystem: Subsystem) -> &'static str {
    match subsystem {
        Subsystem::Player => "player",
        Subsystem::Mixer => "mixer",
        Subsystem::Playlist => "playlist",
        Subsystem::StoredPlaylist => "stored_playlist",
    }
}

/// The first word of a line `split` rejected, to name the command in the
/// `ACK`. Split at the space and without quotes: that is all that can be
/// said of a badly quoted line, and an empty `{}` (what MPD writes) leaves
/// the client with no clue which of its lines was at fault.
fn first_word(raw: &str) -> &str {
    raw.split_whitespace().next().unwrap_or("")
}

/// Writes a response in one go.
///
/// One `write_all` per response and not one per line: a response of 51 lines
/// then costs one system call instead of 51, and nothing can slip in the
/// middle — two responses of the same session are written one after the
/// other by construction, but a half-written response would be read as a
/// complete one by a client that counts its terminators.
async fn write(writer: &mut OwnedWriteHalf, lines: &[String]) -> Result<()> {
    // Exact capacity from the start: without it, flattening a response close
    // to a mebibyte would reallocate it some twenty times by doubling, each
    // time requesting a contiguous block bigger than the previous one.
    // `MAX_RESPONSE` bounds the size of this buffer; this line bounds the
    // number of times it is requested.
    let mut buffer = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in lines {
        buffer.push_str(l);
        buffer.push('\n');
    }
    writer.write_all(buffer.as_bytes()).await?;
    Ok(())
}

/// Writes a **binary** response in one go: the header, the raw bytes, then
/// the terminator.
///
/// The shape is MPD's, byte for byte:
///
/// ```text
/// size: <size of the whole image>
/// type: <mime>            (readpicture only)
/// binary: <size of this chunk>
/// <the raw bytes>
/// OK
/// ```
///
/// The `\n` following the raw bytes is not decorative: it is the one MPD
/// writes (`Response::WriteBinary`), and libmpdclient consumes it before
/// reading the terminator. Omitting it would make `<last byte>OK` read as an
/// unknown line.
///
/// **A single `write_all`, like `write`**, and the same reason: a
/// half-written response would be read as a complete one by a client that
/// counts its terminators. Copying the chunk into the buffer costs at most
/// `MAX_CHUNK_CAP` bytes — sixty-four kibibytes if the client has raised its
/// limit through `binarylimit`, eight otherwise, to compare against the tens
/// of mebibytes the text path had to be forbidden.
///
/// **What this function does not do: allocate the image.** `binary.image` is
/// an `Arc` shared with the state; only the chunk is copied. This is what
/// makes the worst case of a binary connection independent of the cover's
/// size.
async fn write_bytes(writer: &mut OwnedWriteHalf, binary: &Binary) -> Result<()> {
    // Unchecked indexing: it is `commands::cover` that establishes the
    // range, and its contract is that it fits within the image and within
    // the connection's limit, itself capped at `MAX_CHUNK_CAP`. The debug
    // assertion states it rather than silently assuming it, at no cost in
    // production.
    let chunk = &binary.image[binary.chunk.clone()];
    debug_assert!(
        chunk.len() <= MAX_CHUNK_CAP,
        "a chunk exceeds the plugin's cap"
    );
    let binary_line = line("binary", chunk.len());
    let header: usize =
        binary.header.iter().chain(std::iter::once(&binary_line)).map(|l| l.len() + 1).sum();
    // Exact capacity: header, chunk, then `\nOK\n`.
    let mut buffer = Vec::with_capacity(header + chunk.len() + 4);
    for l in binary.header.iter().chain(std::iter::once(&binary_line)) {
        buffer.extend_from_slice(l.as_bytes());
        buffer.push(b'\n');
    }
    buffer.extend_from_slice(chunk);
    buffer.extend_from_slice(b"\nOK\n");
    writer.write_all(&buffer).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Snapshot;
    use ritornello_proto::{Command, Track, Playback, PlayerState};
    // `AsyncReadExt` for the `read_exact` of binary chunks: the test client is
    // the only one in the plugin that reads raw bytes.
    use tokio::io::AsyncReadExt;
    // The session's bounded reader no longer needs this; the test client, for
    // its part, reads lines without a cap to defend.
    use tokio::io::Lines;

    /// A test client: the lines received on one side, the pen on the other.
    struct Client {
        lines: Lines<BufReader<OwnedReadHalf>>,
        writer: OwnedWriteHalf,
    }

    impl Client {
        /// A client on an already-open stream. Separate from `Server::client`:
        /// the rebind tests connect to an address that is not the one the
        /// test `Server` carries.
        fn from_stream(stream: TcpStream) -> Client {
            let (playback, writer) = stream.into_split();
            Client { lines: BufReader::new(playback).lines(), writer }
        }

        async fn connect(address: std::net::SocketAddr) -> Client {
            Client::from_stream(TcpStream::connect(address).await.unwrap())
        }

        async fn send_frame(&mut self, line: &str) {
            self.writer.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        }

        async fn receive(&mut self) -> String {
            self.lines.next_line().await.unwrap().expect("the server closed the connection")
        }

        /// Reads exactly `n` **raw** bytes.
        ///
        /// Through the line reader (`get_mut`) and not directly on the
        /// socket: the bytes following a header are already in the
        /// `BufReader`'s buffer by the time the last header line has been
        /// returned, and reading the socket directly would leave them there —
        /// a test that would hang through no fault of the server's.
        async fn bytes(&mut self, n: usize) -> Vec<u8> {
            let mut buffer = vec![0u8; n];
            self.lines.get_mut().read_exact(&mut buffer).await.unwrap();
            buffer
        }

        /// Replays the sequence of a real client: one request per chunk, the
        /// offset growing, until it holds `size` bytes.
        ///
        /// This is exactly the loop M.A.L.P. and libmpdclient use: the first
        /// response learns the total size, each following one is requested
        /// at the offset of what is already held. The loop's exit does not
        /// depend on any clock or iteration count — only on `size`.
        async fn fetch(&mut self, command: &str, uri: &str) -> Fetched {
            let mut fetched = Fetched { image: Vec::new(), sizes: Vec::new(), mime: None };
            loop {
                self.send_frame(&format!("{command} {uri} {}", fetched.image.len())).await;
                let size = self.integer("size").await;
                let mut header = self.receive().await;
                // `type:` is only there for `readpicture`: it is one more
                // line, before `binary:`, exactly where MPD places it.
                if let Some(mime) = header.strip_prefix("type: ") {
                    fetched.mime = Some(mime.to_string());
                    header = self.receive().await;
                }
                let n: usize = header
                    .strip_prefix("binary: ")
                    .unwrap_or_else(|| panic!("expected binary:, got {header}"))
                    .parse()
                    .unwrap();
                // An empty chunk does not advance the loop: rejecting it here
                // turns a stalling server into an outright failure, rather
                // than a test that spins forever.
                assert!(n > 0, "an empty chunk never ends the fetch");
                fetched.image.extend_from_slice(&self.bytes(n).await);
                fetched.sizes.push(n);
                // The `\n` MPD writes after the raw bytes: read as an empty
                // line. Its absence would make `<last byte>OK` read as one.
                assert_eq!(self.receive().await, "", "a newline follows the raw bytes");
                assert_eq!(self.receive().await, "OK", "each chunk is a complete response");
                if fetched.image.len() >= size {
                    return fetched;
                }
            }
        }

        /// The integer value of an expected `key: number` line.
        async fn integer(&mut self, key: &str) -> usize {
            let l = self.receive().await;
            l.strip_prefix(&format!("{key}: "))
                .unwrap_or_else(|| panic!("expected {key}:, got {l}"))
                .parse()
                .unwrap()
        }

        /// Reads up to and including the terminator: `OK` or an `ACK`.
        /// `list_OK` is not one — that is what makes counting the two
        /// possible.
        async fn response(&mut self) -> Vec<String> {
            let mut received = Vec::new();
            loop {
                let l = self.receive().await;
                let done = l == "OK" || l.starts_with("ACK ");
                received.push(l);
                if done {
                    return received;
                }
            }
        }
    }

    /// What a complete cover fetch produced: the reassembled image, the size
    /// of each chunk received in order, and the MIME type if the server
    /// announced one.
    struct Fetched {
        image: Vec<u8>,
        sizes: Vec<usize>,
        mime: Option<String>,
    }

    struct Server {
        address: std::net::SocketAddr,
        state: Arc<SharedState>,
        /// Kept alive on purpose: dropping the sender would make `listen`
        /// exit its `select!` ("the admin half is gone") and the tests would
        /// no longer exercise the ordinary serving path, only the shutdown
        /// one.
        _config_tx: tokio::sync::watch::Sender<crate::config::Config>,
    }

    /// Binds the listener **in the test** and hands it to the server, as
    /// `register.rs` does for its Unix sockets: the listener therefore
    /// exists before the client connects, and no retry loop or delay is
    /// needed for `connect` to succeed.
    async fn server() -> (Server, mpsc::Receiver<InputMessage>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(SharedState::default());
        let (tx, rx) = mpsc::channel(64);
        let (config_tx, config_rx) =
            tokio::sync::watch::channel(crate::config::Config::default());
        tokio::spawn(listen(listener, config_rx, state.clone(), tx));
        (Server { address, state, _config_tx: config_tx }, rx)
    }

    impl Server {
        async fn client(&self) -> Client {
            Client::connect(self.address).await
        }

        /// A client whose banner has already been swallowed.
        async fn client_ready(&self) -> Client {
            let mut c = self.client().await;
            let banner = c.receive().await;
            assert!(banner.starts_with("OK MPD "), "unexpected banner: {banner}");
            c
        }
    }

    /// A frame that moves only `mixer`.
    fn mixer_frame(volume: u8) -> PlayerState {
        PlayerState { volume, ..Default::default() }
    }

    /// A frame that moves `player` **and** `mixer`: the position moves one,
    /// the volume the other.
    fn player_and_mixer_frame(v: u8) -> PlayerState {
        PlayerState { volume: v, position_s: Some(u32::from(v)), ..Default::default() }
    }

    /// Reads a sleeper's response, pushing frames until it arrives.
    ///
    /// **Without a clock and without an iteration count**, the two forms of
    /// margin this repo has learned not to write any more: the loop stops
    /// when the sleeper answers, and an implementation that never wakes it
    /// makes the test *hang* — an outright block, not a doubtful pass.
    ///
    /// **Two alternating frames, and not one repeated**: each push must be a
    /// real change, otherwise `apply_state`'s deduplication swallows it and
    /// the loop spins forever.
    ///
    /// The loop itself no longer arbitrates any race. It used to arbitrate
    /// one: a frame applied before the session had read its `idle` line was
    /// included in the counters it memorized, and thus invisible to it.
    /// **That was a defect of the session and not a contract of `wait`** —
    /// the reference of an `idle` is now the one the connection carries
    /// since its banner (see `serve`), so a single frame would suffice here.
    /// The loop is kept because it depends on no clock and stays correct in
    /// both cases; what must no longer be done is justifying it by a race
    /// that has been fixed.
    async fn sleeper_response(
        client: &mut Client,
        state: &SharedState,
        frames: [PlayerState; 2],
    ) -> Vec<String> {
        let mut i = 0usize;
        let first = loop {
            tokio::select! {
                // `biased`: as soon as a line is there, take it rather than
                // pushing one more frame.
                biased;
                read = client.lines.next_line() => {
                    break read.unwrap().expect("the server closed the connection");
                }
                () = state.apply_state(frames[i % 2].clone()) => {
                    i += 1;
                    tokio::task::yield_now().await;
                }
            }
        };
        let mut received = vec![first];
        while received.last().map(String::as_str) != Some("OK") {
            received.push(client.receive().await);
        }
        received
    }

    #[tokio::test]
    async fn the_banner_arrives_without_anything_being_asked() {
        let (s, _rx) = server().await;
        let mut c = s.client().await;
        let banner = c.receive().await;
        // Compared against the literal string and not against
        // `ANNOUNCED_VERSION`: against the constant, this test would only
        // verify the formatting, whereas it is the **number** that decides
        // the capabilities a client allows itself. Changing it must be a
        // conscious gesture, not a side effect.
        assert_eq!(banner, "OK MPD 0.23.5");
    }

    #[tokio::test]
    async fn a_command_returns_its_lines_then_ok() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("status").await;
        let received = c.response().await;
        assert_eq!(*received.last().unwrap(), "OK");
        assert!(received.iter().any(|l| l.starts_with("volume: ")), "{received:?}");
        assert!(received.iter().any(|l| l.starts_with("state: ")), "{received:?}");
    }

    #[tokio::test]
    async fn a_command_list_returns_only_one_ok() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("status").await;
        c.send_frame("command_list_end").await;
        let received = c.response().await;
        let ok = received.iter().filter(|l| *l == "OK").count();
        assert_eq!(ok, 1, "a single OK closes the list: {received:?}");
        // And both commands were indeed executed: without that, "a single
        // OK" would be just as true of a list that executes nothing.
        assert_eq!(received.iter().filter(|l| l.starts_with("volume: ")).count(), 2, "{received:?}");
    }

    #[tokio::test]
    async fn command_list_ok_begin_inserts_a_list_ok_per_command() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("command_list_ok_begin").await;
        c.send_frame("status").await;
        c.send_frame("ping").await;
        c.send_frame("command_list_end").await;
        let received = c.response().await;
        assert_eq!(received.iter().filter(|l| *l == "list_OK").count(), 2, "{received:?}");
        assert_eq!(*received.last().unwrap(), "OK");
        // The `list_OK` of a command without lines (`ping`) is the last one
        // before the `OK`: this is what lets a client match each response to
        // its command, empty ones included.
        assert_eq!(received[received.len() - 2], "list_OK", "{received:?}");
    }

    #[tokio::test]
    async fn an_error_in_a_list_interrupts_the_rest() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("nawak").await;
        c.send_frame("status").await;
        c.send_frame("command_list_end").await;
        let received = c.response().await;
        assert_eq!(*received.last().unwrap(), "ACK [5@1] {nawak} unsupported", "{received:?}");
        assert!(!received.iter().any(|l| l == "OK"), "an ACK replaces the OK: {received:?}");
        // **Watch what actually proves what here**: the review got caught
        // out and the previous comment was wrong. `response()` stops at the
        // **first** terminator, and an `ACK` is one: counting the `volume:`
        // lines of this single response therefore proves nothing. A session
        // that kept executing the list after the error would write
        // everything as one block, and `response()` would return exactly the
        // same lines — up to the `ACK` — leaving the following `status`
        // **behind** in the stream.
        //
        // What kills that mutant is what comes next: the following command
        // must receive its own response and nothing else. A leaked `status`
        // shows up here, and the count is taken over **both** responses. Do
        // not "shorten" this test by keeping the count and dropping the
        // `ping`: it is the `ping` that does the work.
        c.send_frame("ping").await;
        let after = c.response().await;
        assert_eq!(after, vec!["OK".to_string()], "leaked response: {after:?}");
        let volumes = received.iter().chain(after.iter()).filter(|l| l.starts_with("volume: ")).count();
        assert_eq!(volumes, 1, "the third status must not have run: {received:?} {after:?}");
    }

    #[tokio::test]
    async fn idle_only_responds_on_change() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle").await;
        let received = sleeper_response(&mut c, &s.state, [mixer_frame(17), mixer_frame(18)]).await;
        // The wakeup names the subsystem and only it, then closes with `OK`.
        assert_eq!(received, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn idle_filters_the_requested_subsystems() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle player").await;
        // Each frame moves `player` **and** `mixer`: the wakeup is therefore
        // certain (no race to arbitrate), and the filter is measured by what
        // the response does *not* name. A session that ignored the requested
        // list would write two `changed:` lines here.
        let received = sleeper_response(
            &mut c,
            &s.state,
            [player_and_mixer_frame(17), player_and_mixer_frame(18)],
        )
        .await;
        assert_eq!(received, vec!["changed: player".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn noidle_returns_control_immediately() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle").await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
        // A bare `OK`, and above all **nothing before it**: this is the
        // clock-free proof that `idle` does not answer on its own. Had it
        // answered without any frame moving, the first line read would be a
        // `changed:`.
        assert_eq!(c.response().await, vec!["OK".to_string()]);
        // And this `status` is there to count responses without a clock: the
        // second one must be **its own**. A session that returned a
        // complacent `OK` to the `idle` (instead of waiting) would have
        // slipped one more response into the stream, and we would read here
        // the `OK` of the `noidle` instead of the `status`'s lines. Without
        // this half, the test passed just as well with an `idle` that
        // answers right away — checked, and that is what got it rewritten.
        let after = c.response().await;
        assert!(after.iter().any(|l| l.starts_with("volume: ")), "{after:?}");
        // And nothing moved in the state: `noidle` cancels a wait, it does
        // not publish a change.
        assert_eq!(s.state.read().await.versions, [0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn a_command_during_a_wait_cancels_the_wait_then_is_executed() {
        // **An accounting test, not a content one.** The client writes two
        // lines (`idle`, `status`) and must receive **two** terminators. This
        // code used to write only one — the `ACK` rejecting the `status` —
        // and the `idle` received none: the client came out permanently
        // shifted by one, every following response read as the one for its
        // previous request. Silent and permanent, where MPD's choice
        // (closing) is loud and self-repairing.
        //
        // The implicit `noidle` is what repairs it: a bare `OK` closes the
        // `idle`, then the command is executed like anywhere else.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle").await;
        c.send_frame("status").await;

        // First terminator: that of the cancelled `idle`. Bare `OK`, no
        // `changed:` — nothing moved, and `noidle` announces nothing anyway.
        assert_eq!(c.response().await, vec!["OK".to_string()]);
        // Second terminator: the `status` response, with its lines.
        let second = c.response().await;
        assert!(second.iter().any(|l| l.starts_with("volume: ")), "{second:?}");
        assert_eq!(*second.last().unwrap(), "OK");
        // And the third request receives **its own** response: this is what
        // proves there is no shift. A `ping` answers with a bare `OK`, so a
        // `status` response lingering in the stream would show up here.
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);

        // ------------------------------------------------------------------
        // **And the neighboring case that must stay at ONE terminator.**
        // Kept in the same test on purpose: separated, a future rework would
        // think one of them redundant. `noidle` is not a request but the
        // cancellation of the one in progress, so `idle` + `noidle` = one
        // `OK`, like MPD. If the fix above made it pass to two, it would
        // have broken the correct case.
        // ------------------------------------------------------------------
        c.send_frame("idle").await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
        assert_eq!(c.response().await, vec!["OK".to_string()], "a single OK for idle + noidle");
        let after = c.response().await;
        assert!(
            after.iter().any(|l| l.starts_with("volume: ")),
            "one terminator too many after noidle: {after:?}"
        );
    }

    #[tokio::test]
    async fn an_unreadable_line_during_a_wait_also_counts_two_terminators() {
        // The same accounting on the other entry of this branch: a badly
        // quoted line is not `noidle` (it does not split), so it is an
        // implicit `noidle` followed by a line that will receive its `ACK`
        // through the ordinary path. Two lines written, two terminators.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle").await;
        c.send_frame(r#"load "France"#).await;

        assert_eq!(c.response().await, vec!["OK".to_string()]);
        assert_eq!(c.response().await, vec!["ACK [2@0] {load} invalid argument".to_string()]);
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_command_list_opened_during_a_wait_is_treated_as_a_list() {
        // The pushed-back line goes back through `serve`'s **full** dispatch,
        // and not through a local reinterpretation: a `command_list_begin`
        // received during a wait therefore opens a real list, whose single
        // `OK` arrives after the cancelled `idle`'s. This is what guarantees
        // no case needs to be duplicated in `wait_idle`.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle").await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("status").await;
        c.send_frame("command_list_end").await;

        assert_eq!(c.response().await, vec!["OK".to_string()], "the cancelled idle gets its terminator");
        let list = c.response().await;
        assert_eq!(list.iter().filter(|l| *l == "OK").count(), 1, "{list:?}");
        assert_eq!(list.iter().filter(|l| l.starts_with("volume: ")).count(), 2, "{list:?}");
    }

    #[tokio::test]
    async fn a_change_between_two_commands_is_reported_by_the_next_idle() {
        // **THE test for this fix.** The session used to memorize the
        // counters in the `Snapshot` of the `idle` command itself, so
        // anything that had moved between the client's previous response and
        // its `idle` line was swallowed — that is, during the only window
        // where an MPD client is not listening. For `stored_playlist`,
        // nothing replays the event before the next sources_catalog change:
        // `listplaylists` stays stale, potentially forever. This is exactly
        // the first trial planned on the device ("disable a source, its list
        // must shrink"), which could therefore fail silently.
        //
        // Without a clock, and **a single frame pushed**: this is what makes
        // the proof conclusive. No change will follow, so a session that
        // re-reads its counters at the `idle` line sleeps forever and this
        // test **hangs** — the intended failure mode. Checked against the
        // old code.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        // One command, its response read up to the terminator: the client is
        // now "between two commands", exactly like a client that has just
        // refreshed its screen.
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);

        // The change arrives now: nobody is waiting.
        s.state.apply_catalog(ritornello_proto::SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![ritornello_proto::Preset { index: 1, name: "FIP".into() }],
            }],
        })
        .await;

        c.send_frame("idle stored_playlist").await;
        assert_eq!(
            c.response().await,
            vec!["changed: stored_playlist".to_string(), "OK".to_string()]
        );
    }

    #[tokio::test]
    async fn a_wakeup_only_consumes_the_subsystems_it_announces() {
        // The fine half of the same mechanism. The wakeup advances the
        // connection's reference **subsystem by subsystem**, just as MPD
        // only clears the flags it just reported: advancing the whole array
        // at once would lose the change of a subsystem this particular
        // `idle` had not asked for, and the defect fixed above would reopen
        // one notch further out.
        //
        // Deterministic and clock-free: the frame is applied **before** the
        // two `idle`s, so each one starts from the prior comparison, never
        // sleeping. An implementation that reset the whole table on the
        // first wakeup would make the second `idle` *hang*.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        // A single frame, which moves `player` **and** `mixer`.
        s.state.apply_state(player_and_mixer_frame(17)).await;

        c.send_frame("idle player").await;
        assert_eq!(c.response().await, vec!["changed: player".to_string(), "OK".to_string()]);

        c.send_frame("idle mixer").await;
        assert_eq!(c.response().await, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn an_announced_wakeup_does_consume_its_counter() {
        // The indispensable counterpart: the reference must *advance*.
        // Without that an `idle` would report the same change forever, and a
        // client that loops on `idle` — that is, all of them — would spin at
        // full speed on the very command meant to spare it that.
        //
        // Proved without a clock: the second `idle` must **wait**, so the
        // following command is a `noidle` whose single `OK` is followed by
        // the `status` response. Had the second `idle` answered on its own,
        // there would be one terminator too many and we would read here the
        // `OK` of the `noidle` instead of the `status`'s lines — the same
        // count as `noidle_returns_control_immediately`.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        s.state.apply_state(mixer_frame(17)).await;

        c.send_frame("idle mixer").await;
        assert_eq!(c.response().await, vec!["changed: mixer".to_string(), "OK".to_string()]);

        c.send_frame("idle mixer").await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
        let after = c.response().await;
        assert!(
            after.iter().any(|l| l.starts_with("volume: ")),
            "the second idle answered on its own: {after:?}"
        );
    }

    #[tokio::test]
    async fn two_clients_do_not_get_in_each_others_way() {
        // THE architecture test. If `accept_loop` served connections one
        // after another instead of one task per client, B would not even get
        // its banner while A is asleep, and this test would **block** — the
        // outright failure mode this codebase prefers over a clock margin.
        let (s, _rx) = server().await;
        let mut a = s.client_ready().await;
        a.send_frame("idle").await;

        let mut b = s.client_ready().await;
        b.send_frame("status").await;
        let received = b.response().await;
        assert_eq!(*received.last().unwrap(), "OK", "{received:?}");
        assert!(received.iter().any(|l| l.starts_with("volume: ")), "{received:?}");

        // And A really was asleep: without this half, the test would pass
        // just as well with an A whose session is dead — the wakeup proves
        // it was alive and waiting while B was being served.
        let wakeup = sleeper_response(&mut a, &s.state, [mixer_frame(17), mixer_frame(18)]).await;
        assert_eq!(wakeup, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn an_action_command_arrives_on_the_input_channel() {
        let (s, mut rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("next").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.cmd, Command::Next);
        assert!(!msg.held, "a network command is never held");
        // Exactly one: a duplicated command would skip two stations.
        assert!(rx.try_recv().is_err(), "a single command for a single next");
    }

    #[tokio::test]
    async fn a_read_only_command_emits_nothing_on_the_channel() {
        let (s, mut rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("status").await;
        c.response().await;
        // The response has arrived, so the command is fully processed: had
        // `status` emitted anything, it would already be on the channel. No
        // clock is needed to assert that.
        assert!(rx.try_recv().is_err(), "status asks nothing of the device");
    }

    #[tokio::test]
    async fn a_closed_channel_closes_the_session_without_acknowledging_the_switch() {
        // The "push then acknowledge" order is measured here and nowhere
        // else: the channel refuses (receiver dropped), so nothing was
        // emitted, so nothing must have been acknowledged. A session that
        // called `acknowledge_optimistic` first would set volume 30 in the
        // shared state and have `status` publish it to every other client —
        // a switch the core never received.
        let (s, rx) = server().await;
        drop(rx);
        let mut c = s.client_ready().await;
        c.send_frame("setvol 30").await;
        assert!(
            c.lines.next_line().await.unwrap().is_none(),
            "a dead input half closes the session"
        );
        assert_eq!(s.state.read().await.state.volume, 0, "nothing is acknowledged if the channel refused");
        assert_eq!(s.state.read().await.versions, [0, 0, 0, 0], "and nobody is woken");
    }

    #[tokio::test]
    async fn idle_in_a_list_is_rejected_at_its_rank() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("idle").await;
        let received = c.response().await;
        // The index is `idle`'s rank in the list (1), not 0: a client
        // that groups ten commands must know which one was rejected.
        assert_eq!(received, vec!["ACK [5@1] {idle} not allowed in command list".to_string()]);
        // Rejected **at accumulation**: the preceding `status` was not
        // executed, so no `volume:` line accompanies the ACK.
        assert!(!received.iter().any(|l| l.starts_with("volume: ")), "{received:?}");
        // And the list state was cleared: the following command answers alone.
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_list_with_no_end_is_bounded() {
        // A list accumulates in memory without executing anything: without a
        // cap, a client that never sends its `command_list_end` grows a
        // `Vec` until a Pi's memory is exhausted. The rejection arrives at
        // the cap's rank and clears the list state.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        let mut batch = String::from("command_list_begin\n");
        for _ in 0..=MAX_LIST_COMMANDS {
            batch.push_str("ping\n");
        }
        c.writer.write_all(batch.as_bytes()).await.unwrap();
        let received = c.response().await;
        assert_eq!(
            received,
            vec![format!("ACK [5@{MAX_LIST_COMMANDS}] {{ping}} list too large")],
            "{received:?}"
        );
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_line_longer_than_the_cap_closes_the_connection() {
        // The plugin's last unbounded surface, and it is reachable without a
        // password from the whole local network: a client that sends bytes
        // without ever sending a `\n`. Without a cap, the session accumulates
        // until the allocator gives up — on a Pi with one gigabyte shared
        // with mpv, that takes the music down and not only the plugin.
        //
        // Without a clock: the bound is measured by the fact that the
        // connection **ends**. Without a cap, this `next_line` would wait
        // for the `\n` forever and the test would hang — checked, and that
        // is the intended failure mode.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        let filler = vec![b'a'; MAX_LINE + 1];
        // The write can fail if the server has already closed: that is an
        // acceptable end and not a test failure, hence the ignored result.
        let _ = c.writer.write_all(&filler).await;
        assert!(
            c.lines.next_line().await.unwrap().is_none(),
            "a line past the cap closes the connection, without an ACK"
        );
    }

    #[tokio::test]
    async fn a_line_long_but_under_the_cap_is_processed() {
        // The counterpart of the previous test: a cap that cuts a legitimate
        // line would be worse than no cap. The longest plausible line of the
        // protocol is a quoted name, and it must arrive whole — here it
        // measures exactly the cap.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        let name = "a".repeat(MAX_LINE - "load \"\"".len());
        c.send_frame(&format!("load \"{name}\"")).await;
        // `load` rejects every name for lack of a sources_catalog (Task 13),
        // and that is precisely a response that proves the line was **split
        // whole**: an `ACK 2` or a close would say it had been truncated.
        assert_eq!(c.response().await, vec!["ACK [50@0] {load} no such playlist".to_string()]);
    }

    #[tokio::test]
    async fn a_line_terminated_by_crlf_is_read_without_the_carriage_return() {
        // Clients written on Windows terminate with `\r\n`. The hand-written
        // reader had to reproduce what `Lines` did for us, and nothing said
        // so: without the `\r` stripped, the command would be `ping\r`,
        // hence an `ACK 5` — a regression no existing test caught.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.writer.write_all(b"ping\r\n").await.unwrap();
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_last_line_without_a_newline_is_processed_before_closing() {
        // A client that sends its command then closes its write half must
        // see it processed: end of stream terminates the line. That is what
        // `Lines` did, and the "buffer non-empty at EOF" path of the new
        // reader has no other witness than this test.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.writer.write_all(b"ping").await.unwrap();
        // `shutdown` and not a `drop`: the client's read half must stay open
        // to read the response.
        c.writer.shutdown().await.unwrap();
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_line_past_the_cap_with_a_newline_also_closes() {
        // The cap is checked in **both** arms of the reader, and the
        // previous test only visits one (the read chunk contains no `\n`).
        // This one visits the other: the line exceeds the cap *and*
        // terminates properly. Without this case, removing the check from
        // the `Some` arm let everything through — a cap nobody exercises is
        // a cap that gets removed by inattention.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        // Exactly `MAX_LINE` bytes without `\n`: legal, the buffer keeps them.
        c.writer.write_all(&vec![b'a'; MAX_LINE]).await.unwrap();
        // Then one byte too many, this time followed by its line ending:
        // this is the `Some` arm that must reject, counting what was already
        // accumulated.
        let _ = c.writer.write_all(b"b\n").await;
        assert!(
            c.lines.next_line().await.unwrap().is_none(),
            "a line past the cap closes the connection, even terminated"
        );
    }

    #[tokio::test]
    async fn an_empty_line_is_rejected_without_closing() {
        // A bare `\n`. `handle` already knows to reject it (it is total by
        // construction), but no session test showed it end to end: the
        // session could swallow it silently, and a client waiting for one
        // response per line would be left hanging.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.writer.write_all(b"\n").await.unwrap();
        assert_eq!(c.response().await, vec!["ACK [5@0] {} unsupported".to_string()]);
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_response_too_large_is_rejected_without_closing() {
        // The amplifier: `MAX_LIST_COMMANDS` bounds the commands, not what
        // they **produce**. A list of `playlistinfo` on a queue of 255
        // entries returns some fifteen kibibytes per command, and the whole
        // response used to be flattened into a single `String` before the
        // `write_all` — hence a contiguous allocation of several tens of
        // mebibytes, requested from a Pi whose memory is fragmented. 26 KiB
        // of input was enough.
        //
        // The rejection arrives **before** any write, so it replaces the
        // response instead of being added to it: a single terminator, and
        // the connection lives.
        let (s, _rx) = server().await;
        s.state
            .apply_state(PlayerState {
                source: "cd".to_string(),
                preset_count: Some(255),
                ..Default::default()
            })
            .await;
        let mut c = s.client_ready().await;
        let mut batch = String::from("command_list_begin\n");
        for _ in 0..100 {
            batch.push_str("playlistinfo\n");
        }
        batch.push_str("command_list_end\n");
        c.writer.write_all(batch.as_bytes()).await.unwrap();
        let received = c.response().await;
        assert_eq!(received.len(), 1, "the rejection replaces the composed response: {received:?}");
        // The exact index depends on the byte arithmetic (some fifteen
        // kibibytes per command, a one-mebibyte cap): what matters is that
        // it names the command that overran and its rank in the batch.
        let rejection = &received[0];
        assert!(rejection.starts_with("ACK [5@"), "{rejection}");
        assert!(rejection.ends_with("] {playlistinfo} response too large"), "{rejection}");
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn a_list_heavy_in_bytes_is_rejected_well_before_the_count() {
        // The other half of the same hole: an accumulated line can
        // legitimately weigh `MAX_LINE`, so 2048 commands bounded **by
        // count** weighed 16 MiB per connection. Here thirty-two lines of
        // 8 KiB land *exactly* on the 256 KiB — the cap rejects past it, not
        // at equality — so it is the thirty-third that crosses it, and the
        // loop sends one more for that reason. Thirty-three is very far from
        // the 2048 commands: the bound rejecting here is indeed the
        // byte-based one and not the count-based one.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("command_list_begin").await;
        let mut batch = String::new();
        for _ in 0..MAX_LIST_BYTES.div_ceil(MAX_LINE) + 1 {
            batch.push_str("ping ");
            batch.push_str(&"a".repeat(MAX_LINE - 6));
            batch.push('\n');
        }
        c.writer.write_all(batch.as_bytes()).await.unwrap();
        let received = c.response().await;
        assert_eq!(received.len(), 1, "{received:?}");
        assert!(received[0].starts_with("ACK [5@"), "{received:?}");
        assert!(received[0].ends_with("] {ping} list too large"), "{received:?}");
        // The list state is cleared: the following command answers alone.
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn past_the_session_cap_a_connection_is_rejected_at_once() {
        // The multiplier: every other cap bounds one connection, and the
        // number of connections was not bounded. A client that leaks its
        // connections — that reopens one on every network recovery without
        // closing the previous one — gets here by accident, with no hostile
        // script whatsoever.
        //
        // Without a clock, and the order is guaranteed by the banner: it is
        // written by `serve`, hence *after* the slot is taken. Having read
        // `MAX_SESSIONS` banners proves the `MAX_SESSIONS` slots are taken,
        // and the following connection is therefore indeed the one that
        // overflows.
        let (s, _rx) = server().await;
        let mut open = Vec::new();
        for _ in 0..MAX_SESSIONS {
            open.push(s.client_ready().await);
        }
        // The one too many: accepted by the kernel (the port is still
        // listening), then closed at once by `accept_loop`. No banner, hence
        // end of stream.
        let mut rejected = s.client().await;
        assert!(
            rejected.lines.next_line().await.unwrap().is_none(),
            "past the cap, the connection must be closed without a banner"
        );
        // And the already-open sessions still serve: the cap rejects new
        // ones, it does not degrade the old ones. The first and the last,
        // because a badly wired cap readily breaks one of the two ends.
        for index in [0, MAX_SESSIONS - 1] {
            open[index].send_frame("ping").await;
            assert_eq!(open[index].response().await, vec!["OK".to_string()]);
        }
    }

    #[tokio::test]
    async fn a_settings_change_rebinds_the_server_without_a_restart() {
        // **The owner's request**: no longer having to restart the plugin by
        // hand after changing the port on the admin page.
        //
        // Without a clock, like `a_client_that_leaves_returns_its_slot`: the
        // loop retries until the new port answers, and nothing stops it but
        // that success. An implementation that never rebound would make the
        // test *hang*, which is the intended failure mode — and not a
        // guessed delay that would become a flake under load.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_address = listener.local_addr().unwrap();
        // A free port, chosen by the kernel then released: this is the only way
        // to name one that is not already taken on the test machine.
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let new_address = probe.local_addr().unwrap();
        drop(probe);

        let state = Arc::new(SharedState::default());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64);
        let (config_tx, config_rx) = tokio::sync::watch::channel(crate::config::Config {
            listen: "127.0.0.1".into(),
            port: old_address.port(),
        });
        tokio::spawn(listen(listener, config_rx, state, cmd_tx));

        // The old port serves fine before any change.
        let mut before = Client::connect(old_address).await;
        assert!(before.receive().await.starts_with("OK MPD "));

        config_tx
            .send(crate::config::Config { listen: "127.0.0.1".into(), port: new_address.port() })
            .unwrap();

        let banner = loop {
            if let Ok(stream) = TcpStream::connect(new_address).await {
                let mut c = Client::from_stream(stream);
                break c.receive().await;
            }
            tokio::task::yield_now().await;
        };
        assert!(banner.starts_with("OK MPD "), "unexpected banner: {banner}");

        // And the already-open session was not cut: it holds its own stream,
        // which closing the listener does not touch. This is the difference
        // with a real MPD restart, and it is intentional.
        before.send_frame("ping").await;
        assert_eq!(before.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn an_impossible_port_leaves_the_server_where_it_was() {
        // A faulty setting — port already taken, address absent from the
        // machine — must not make the MPD server unreachable. The old
        // listener is only released once the new one is bound.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_address = listener.local_addr().unwrap();
        let state = Arc::new(SharedState::default());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64);
        let (config_tx, config_rx) = tokio::sync::watch::channel(crate::config::Config {
            listen: "127.0.0.1".into(),
            port: old_address.port(),
        });
        tokio::spawn(listen(listener, config_rx, state, cmd_tx));

        // An address no interface carries: the `bind` fails.
        config_tx
            .send(crate::config::Config { listen: "192.0.2.1".into(), port: 6600 })
            .unwrap();

        // The server still answers where it was answering. Clock-free loop,
        // same reason as the test above: it is success that stops it.
        let banner = loop {
            if let Ok(stream) = TcpStream::connect(old_address).await {
                let mut c = Client::from_stream(stream);
                break c.receive().await;
            }
            tokio::task::yield_now().await;
        };
        assert!(banner.starts_with("OK MPD "), "unexpected banner: {banner}");
    }

    #[tokio::test]
    async fn a_client_that_leaves_returns_its_slot() {
        // The indispensable counterpart: if the permit did not leave with
        // the task, the device would refuse everyone after sixteen
        // connections in the process's lifetime — a failure that would only
        // show up after days, and that would look like a network defect.
        //
        // Without a clock: the loop retries until the slot is returned, and
        // nothing stops it but that success. It is necessary because nothing
        // orders the client's closing with the moment the server session
        // notices it; an implementation that never returned the slot makes
        // the test *hang*, which is the intended failure mode.
        let (s, _rx) = server().await;
        let mut open = Vec::new();
        for _ in 0..MAX_SESSIONS {
            open.push(s.client_ready().await);
        }
        // The first one leaves for good: both its halves are dropped.
        open.remove(0);
        let banner = loop {
            let mut candidate = s.client().await;
            if let Some(line) = candidate.lines.next_line().await.unwrap() {
                break line;
            }
        };
        assert!(banner.starts_with("OK MPD "), "unexpected banner: {banner}");
    }

    #[tokio::test]
    async fn an_unreadable_line_does_not_close_the_connection() {
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame(r#"load "France"#).await;
        let received = c.response().await;
        assert_eq!(received, vec!["ACK [2@0] {load} invalid argument".to_string()]);
        // The following client does not have to reconnect.
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn an_unreadable_line_in_a_list_abandons_it() {
        // The list can no longer be executed as the client wrote it:
        // executing it at `command_list_end` would run a batch amputated of
        // the rejected command. It is therefore abandoned, and the following
        // `command_list_end` is rejected as a command outside a list — a
        // client then knows its batch did not take place.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame(r#"load "France"#).await;
        assert_eq!(c.response().await, vec!["ACK [2@1] {load} invalid argument".to_string()]);
        c.send_frame("command_list_end").await;
        assert_eq!(
            c.response().await,
            vec!["ACK [5@0] {command_list_end} unsupported".to_string()]
        );
    }

    #[tokio::test]
    async fn close_answers_ok_then_closes() {
        // A deliberate decision: MPD writes nothing before closing, we
        // answer. See the comment on `Outcome::Close` in `execute`.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("close").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
        assert!(c.lines.next_line().await.unwrap().is_none(), "close must close");
    }

    #[tokio::test]
    async fn subsystem_names_are_the_ones_idle_accepts() {
        // `subsystem_name` is the inverse of `commands.rs`'s table, and
        // nothing ties the two together at compile time: a hyphenated
        // `stored-playlist` here would announce a subsystem no client could
        // ask for again. Verify it by passing each name to `idle`.
        for subsystem in [Subsystem::Player, Subsystem::Mixer, Subsystem::Playlist, Subsystem::StoredPlaylist] {
            let args = vec!["idle".to_string(), subsystem_name(subsystem).to_string()];
            assert_eq!(
                handle(&Snapshot::default(), 0, &args, MAX_CHUNK),
                Outcome::Wait(vec![subsystem]),
                "subsystem_name({subsystem:?}) is not a name idle accepts"
            );
        }
    }

    #[tokio::test]
    async fn an_idle_with_no_known_subsystem_is_not_an_immediate_ok() {
        // `idle database` only names subsystems this plugin never emits: the
        // subsystem list is empty, and `Outcome::Wait`'s contract says that
        // is a wait **without end**, not an immediate `OK`. An `OK` would
        // make the client loop at full speed on the very command meant to
        // spare it that.
        //
        // Proved without a clock, **by counting terminators**: `idle` +
        // `noidle` are worth only one, so the second response read is the
        // `status`'s. A session that returned `OK` right away would have
        // written one more (its own, then the one for the `noidle` received
        // outside a wait), and we would read here a bare `OK` instead of the
        // `status`'s lines.
        //
        // The discriminant changed with the implicit `noidle`: sending
        // `status` no longer distinguishes anything by itself, since a
        // cancelled wait now writes `OK` then the `status` response —
        // exactly what an `idle` answering right away would also produce.
        let (s, _rx) = server().await;
        let mut c = s.client_ready().await;
        c.send_frame("idle database").await;
        // Frames that move every counter: none concerns the requested
        // subsystems (there are none), so none should wake it.
        s.state.apply_state(player_and_mixer_frame(17)).await;
        s.state.apply_state(player_and_mixer_frame(18)).await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
        let after = c.response().await;
        assert!(
            after.iter().any(|l| l.starts_with("volume: ")),
            "idle database answered on its own: {after:?}"
        );
    }

    // ------------------------------------------------------------------
    // Covers, on a real socket
    // ------------------------------------------------------------------

    /// The `href` the state frame publishes, and that the cover frame
    /// reuses.
    const HREF: &str = "/api/cover/1a2b3c";

    /// The URI our `currentsong` publishes for the state below.
    const CURRENT_URI: &str = "ritornello://radio/2";

    /// A size that is not a multiple of `MAX_CHUNK`: three chunks, the last
    /// one shorter than the others.
    const SIZE: usize = MAX_CHUNK * 2 + 1234;

    /// The state frame **as the core emits it when a cover exists**: it
    /// carries the `cover_href`, and that is what the cover frame will
    /// reuse. A frame without `cover_href` accompanied by a cover does not
    /// exist on the producer's side, and a test that used one would prove
    /// an impossible causality.
    fn frame_with_cover() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 40,
            playback: Playback::Playing,
            preset: Some(2),
            preset_count: Some(3),
            preset_name: Some("France Inter".into()),
            track: Track {
                title: Some("So What".into()),
                cover_href: Some(HREF.to_string()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Pushes the state **then** the cover, in that order: this is the
    /// core's order (`display_relay` sends the state before the bytes), and
    /// the reverse would leave the plugin in a state it never sees in
    /// production.
    async fn with_cover(state: &SharedState, size: usize) -> Vec<u8> {
        let cover = crate::state::test_cover(HREF, size);
        state.apply_state(frame_with_cover()).await;
        state.apply_cover(cover.clone()).await;
        cover.bytes
    }

    #[tokio::test]
    async fn albumart_returns_the_whole_image_and_it_reassembles_identically() {
        // **The central test of this task.** It replays the sequence of a
        // real client on a real socket, and it does not assert "something
        // arrived": it compares the reassembled bytes to the ones that were
        // pushed. A split that skips, duplicates or shifts a single byte
        // fails here — and the image is noise, so nothing can mask it.
        let (s, _rx) = server().await;
        let expected = with_cover(&s.state, SIZE).await;
        let mut c = s.client_ready().await;

        let r = c.fetch("albumart", CURRENT_URI).await;

        assert_eq!(r.image.len(), SIZE, "reassembled size");
        assert_eq!(r.image, expected, "the bytes must arrive intact");
        // Three chunks: two full ones, then the remainder. This is the proof
        // that the growing offset is honored (two more requests than the
        // first) and that the last chunk is shorter than the others.
        assert_eq!(r.sizes, vec![MAX_CHUNK, MAX_CHUNK, 1234]);
        // `albumart` does not announce a MIME type, unlike `readpicture`.
        assert_eq!(r.mime, None);
        // And the connection stays usable after a binary response: the
        // bytes path must not leave the session misaligned.
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn readpicture_returns_the_same_bytes_and_announces_the_type() {
        // M.A.L.P. tries one, then the other: both must succeed, and on the
        // same image. Only `type:` distinguishes them, as with MPD.
        let (s, _rx) = server().await;
        let expected = with_cover(&s.state, SIZE).await;
        let mut c = s.client_ready().await;

        let r = c.fetch("readpicture", CURRENT_URI).await;

        assert_eq!(r.image, expected);
        assert_eq!(r.mime.as_deref(), Some("image/jpeg"));
    }

    #[tokio::test]
    async fn an_image_shorter_than_a_chunk_fits_in_a_single_round_trip() {
        // The real case and not the edge case: the measured cover from the
        // Cover Art Archive is 75 KiB, but a thumbnail can fit under a
        // chunk's 8 KiB. A single request, a single chunk, complete.
        let (s, _rx) = server().await;
        let expected = with_cover(&s.state, 1000).await;
        let mut c = s.client_ready().await;

        let r = c.fetch("albumart", CURRENT_URI).await;

        assert_eq!(r.sizes, vec![1000]);
        assert_eq!(r.image, expected);
    }

    #[tokio::test]
    async fn an_offset_past_the_end_is_rejected_without_closing() {
        let (s, _rx) = server().await;
        with_cover(&s.state, SIZE).await;
        let mut c = s.client_ready().await;

        c.send_frame(&format!("albumart {CURRENT_URI} {}", SIZE + 1)).await;

        assert_eq!(
            c.response().await,
            vec!["ACK [2@0] {albumart} Offset too large".to_string()]
        );
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn without_a_cover_both_commands_reject_and_the_connection_survives() {
        // The ordinary case: a stream with no image. The client must receive
        // a readable rejection and be able to keep talking — this is the
        // rejection that makes it switch to the other name, then give up
        // cleanly.
        //
        // **`cover_href: None`, and that detail is the whole test.** The
        // device announces no image, so the rejection is final and must
        // fall **right away**: `wait_cover`'s new wait only covers the
        // window where an image *has been announced* and has not yet
        // arrived. A frame carrying `cover_href` here — what this test used
        // to do — described, on the contrary, exactly that window, and the
        // immediate rejection it locked in was precisely the defect to fix.
        let (s, _rx) = server().await;
        let mut frame = frame_with_cover();
        frame.track.cover_href = None;
        s.state.apply_state(frame).await;
        let mut c = s.client_ready().await;

        for name in ["albumart", "readpicture"] {
            c.send_frame(&format!("{name} {CURRENT_URI} 0")).await;
            assert_eq!(
                c.response().await,
                vec![format!("ACK [50@0] {{{name}}} No file exists")]
            );
        }
        c.send_frame("ping").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn binarylimit_changes_this_connections_chunk_size() {
        // **What the command is really for.** A cover used to be fetched in
        // 8 KiB chunks, MPD's default value: a 500 KiB image required
        // sixty-two round trips. A client that announces it can accept more
        // must receive more — and the value only holds for **its own**
        // connection.
        let (s, _rx) = server().await;
        let expected = with_cover(&s.state, SIZE).await;
        let mut c = s.client_ready().await;

        c.send_frame("binarylimit 32768").await;
        assert_eq!(c.response().await, vec!["OK".to_string()]);

        let r = c.fetch("albumart", CURRENT_URI).await;
        assert_eq!(r.image, expected);
        // SIZE fits under 32 KiB: a single chunk, where the default asked
        // for three.
        assert_eq!(r.sizes, vec![SIZE], "the requested chunk size must be honored");

        // A second client, which asked nothing, keeps the default: the
        // limit is a fact about the connection.
        let mut other = s.client_ready().await;
        let r2 = other.fetch("albumart", CURRENT_URI).await;
        assert_eq!(r2.sizes.first(), Some(&MAX_CHUNK));
    }

    #[tokio::test]
    async fn a_cover_announced_but_not_yet_arrived_is_awaited_then_served() {
        // **The fix for "the cover disappears on track change".** The core
        // sends the state first, the bytes after: the client is woken by
        // this frame and requests the image right away, while the plugin
        // still holds the previous one — or nothing at all. It used to get
        // "No file exists", and M.A.L.P., which memorizes the absence per
        // track, would never ask again.
        //
        // Here the request arrives **before** the bytes, and must still
        // succeed.
        let (s, _rx) = server().await;
        s.state.apply_state(frame_with_cover()).await;
        let mut c = s.client_ready().await;

        let state = s.state.clone();
        let expected = crate::state::test_cover(HREF, SIZE).bytes;
        // The cover arrives while the request is waiting. A separate task,
        // because that is exactly the real concurrency: two distinct
        // channels, one behind the other.
        tokio::spawn(async move {
            state.apply_cover(crate::state::test_cover(HREF, SIZE)).await;
        });

        let r = c.fetch("albumart", CURRENT_URI).await;
        assert_eq!(r.image, expected, "the awaited image must eventually be served");
    }

    #[tokio::test(start_paused = true)]
    async fn a_cover_announced_that_never_arrives_eventually_gets_rejected() {
        // The counterpart: the wait is **bounded**. Without this bound, an
        // image that never arrives — a sleeping share, a 404 from the Cover
        // Art Archive — would leave the client suspended forever on a
        // command it is waiting for a response to.
        //
        // Simulated clock: tokio advances virtual time as soon as everything
        // is waiting, so this test does not cost the real three seconds and
        // assumes no execution duration.
        let (s, _rx) = server().await;
        s.state.apply_state(frame_with_cover()).await;
        let mut c = s.client_ready().await;

        c.send_frame(&format!("albumart {CURRENT_URI} 0")).await;

        assert_eq!(
            c.response().await,
            vec!["ACK [50@0] {albumart} No file exists".to_string()],
            "the wait must eventually return the ordinary rejection"
        );
    }

    #[tokio::test]
    async fn a_binary_response_in_a_list_is_rejected_at_its_rank() {
        // MPD allows it, we do not: see the justification in place in
        // `serve`. The rejection arrives **at accumulation**, so the
        // preceding `status` was not executed — that is what the absence of
        // `volume:` proves.
        let (s, _rx) = server().await;
        with_cover(&s.state, SIZE).await;
        let mut c = s.client_ready().await;

        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame(&format!("albumart {CURRENT_URI} 0")).await;
        let received = c.response().await;

        assert_eq!(received, vec!["ACK [5@1] {albumart} not allowed in command list".to_string()]);
        assert!(!received.iter().any(|l| l.starts_with("volume: ")), "{received:?}");
        // The list state was cleared, and the command does answer outside a
        // list: the rejection does not condemn the command, only its
        // packaging.
        let r = c.fetch("albumart", CURRENT_URI).await;
        assert_eq!(r.image.len(), SIZE);
    }

    #[tokio::test]
    async fn a_cover_that_arrives_wakes_a_sleeper_on_player() {
        // The end-to-end wakeup, on a real socket. It is **necessary** and
        // not cosmetic: the core sends the state first, so a client woken by
        // the state frame alone requests its image too early and gets a
        // rejection. Without this second wakeup, it would never know the
        // image had arrived.
        //
        // Without a clock: the loop pushes covers until the sleeper
        // responds, and an implementation that does not wake it makes the
        // test *hang*.
        let (s, _rx) = server().await;
        s.state.apply_state(frame_with_cover()).await;
        let mut c = s.client_ready().await;
        c.send_frame("idle player").await;

        let mut i = 0usize;
        let first = loop {
            tokio::select! {
                biased;
                read = c.lines.next_line() => {
                    break read.unwrap().expect("the server closed the connection");
                }
                // Two alternating sizes: each push is therefore a real
                // change, which deduplication cannot swallow.
                () = s.state.apply_cover(
                    crate::state::test_cover(HREF, 1000 + (i % 2) * 500),
                ) => {
                    i += 1;
                    tokio::task::yield_now().await;
                }
            }
        };
        assert_eq!(first, "changed: player");
        assert_eq!(c.receive().await, "OK");
    }
}
