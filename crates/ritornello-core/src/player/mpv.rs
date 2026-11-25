use crate::player::Progress;
use crate::types::Event;
use anyhow::{bail, Context, Result};
use ritornello_proto::{Lyrics, Track};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

pub struct MpvIpc {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>,
    next_id: AtomicU64,
    /// Keeps the events of a `play_aside` load away from the core. A plain
    /// mutex: never held across an `await`.
    gate: Arc<std::sync::Mutex<AsideGate>>,
    /// Counts mpv's `audio-reconfig` events, which the core never sees: the
    /// end of a paused chime waits for the one of its chain rewrite
    /// (`end_sound_while_paused`).
    reconfigs: Arc<tokio::sync::watch::Sender<u64>>,
}

impl MpvIpc {
    pub fn from_stream(stream: UnixStream, events: mpsc::Sender<Event>) -> Arc<Self> {
        let (read, write) = stream.into_split();
        let ipc = Arc::new(Self {
            writer: Mutex::new(write),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            gate: Arc::new(std::sync::Mutex::new(AsideGate::default())),
            reconfigs: Arc::new(tokio::sync::watch::Sender::new(0)),
        });
        let pending = ipc.pending.clone();
        let gate = ipc.gate.clone();
        let reconfigs = ipc.reconfigs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            // True until the first `idle-active` notification.
            //
            // `observe_property` immediately returns the current value, and
            // mpv is launched as an **idle** daemon: this first value is
            // therefore always `true`, and it describes a starting state,
            // not a playback stop. The core, for its part, reads
            // `PlaybackIdle` as the end of what was playing — it sets
            // `playback = false` and notifies `Stop` to the Source.
            //
            // Measured in practice: the event waits in the channel while
            // startup launches the first playback, and it is processed
            // right after. On finite content (a file), nothing catches it
            // — no more "listening", rewind and forward greyed out,
            // position absent, until a play/pause reloads everything from
            // the start. A stream would go back through the restart and
            // mask the defect.
            let mut first_idle = true;
            // Journal only, never an event: see `Stall`.
            let mut stall = Stall::default();
            let mut path = String::new();
            'pump: while let Ok(Some(line)) = lines.next_line().await {
                let v = match serde_json::from_str::<Value>(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("non-JSON mpv line ignored: {e}");
                        continue;
                    }
                };
                // Whether this line is the observation's initial
                // `idle-active: true` (see `first_idle`). Decided here, with
                // the rest of the per-line bookkeeping, so that it advances
                // even for a line the `AsideGate` swallows.
                let mut initial_idle = false;
                if v["event"] == json!("property-change") {
                    match (v["name"].as_str(), &v["data"]) {
                        (Some("idle-active"), Value::Bool(true)) => {
                            initial_idle = std::mem::replace(&mut first_idle, false);
                        }
                        // Entering playback also consumes the right to
                        // swallow: if mpv announces activity first, the
                        // `true` that follows is a genuine stop.
                        (Some("idle-active"), Value::Bool(false)) => first_idle = false,
                        (Some("path"), Value::String(p)) => path.clone_from(p),
                        (Some("paused-for-cache"), data) => {
                            match stall.change(data, std::time::Instant::now()) {
                                Some(StallNote::Began) => {
                                    tracing::warn!("playback stalled: no data from {path}")
                                }
                                Some(StallNote::Ended(lasted)) => tracing::warn!(
                                    "playback stall over after {:.1} s without data from {path}",
                                    lasted.as_secs_f64()
                                ),
                                None => {}
                            }
                        }
                        _ => {}
                    }
                }
                if v["event"] == json!("audio-reconfig") {
                    reconfigs.send_modify(|n| *n = n.wrapping_add(1));
                }
                if let Some(id) = v.get("request_id").and_then(Value::as_u64) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let res = if v["error"] == json!("success") {
                            Ok(v.get("data").cloned().unwrap_or(Value::Null))
                        } else {
                            Err(anyhow::anyhow!("mpv: {}", v["error"]))
                        };
                        let _ = tx.send(res);
                    }
                    continue;
                }
                // After the bookkeeping above, so that it still advances for
                // a swallowed line, and before anything reaches the core.
                let replay = {
                    let mut gate = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !gate.admit(&v) {
                        continue;
                    }
                    gate.take_replay()
                };
                for v in std::iter::once(v).chain(replay) {
                    if !(v["event"] == json!("property-change")
                        || v["event"] == json!("file-loaded")
                        || v["event"] == json!("start-file")
                        || v["event"] == json!("playback-restart"))
                    {
                        continue;
                    }
                    let ev = if v["event"] == json!("start-file") {
                        Some(Event::FileStarting)
                    } else if v["event"] == json!("playback-restart") {
                        Some(Event::PlaybackRestarted)
                    } else if v["event"] == json!("file-loaded") {
                        // Documented by mpv (read from its own manual, not
                        // measured against a real running mpv — no session
                        // available here to confirm it) as sent "after a
                        // file has been loaded and playback has started".
                        // This is what `played_since_play` now waits for
                        // (see `Event::PlaybackActive`'s own doc, and the
                        // core's `last_pass_reopen` for the second, timed
                        // safety net this project keeps regardless): unlike
                        // a property, this event fires on *every* load,
                        // including a reload issued while mpv was already
                        // playing something else — which `idle-active`
                        // turning false, used for this before, could miss
                        // entirely (see that arm's own doc just below).
                        Some(Event::PlaybackActive)
                    } else {
                        match (v["name"].as_str(), &v["data"]) {
                        (Some("media-title"), Value::String(t)) => Some(Event::Title(t.clone())),
                        // One property, two layers: the ICY header of a
                        // stream, or the tags of a file. `file_tags` stays
                        // silent as soon as an ICY key is present, so the
                        // two branches are exclusive — the order below is
                        // not a disguised priority.
                        (Some("metadata"), data) => icy_title(data)
                            .map(Event::IcyTitle)
                            .or_else(|| file_tags_event(data)),
                        // The observation's initial value is swallowed (see
                        // `first_idle`); the following ones follow a
                        // playback and are true stops, including the end
                        // of a playlist.
                        (Some("idle-active"), Value::Bool(true)) => {
                            if initial_idle { None } else { Some(Event::PlaybackIdle) }
                        }
                        (Some("idle-active"), Value::Bool(false)) => {
                            // `first_idle` is already cleared above.
                            //
                            // No longer the source of `Event::PlaybackActive`
                            // itself (see `file-loaded` above): read from
                            // mpv's own source (unmeasured — no real mpv
                            // session available here to confirm it against),
                            // this property does not toggle again on a
                            // reload issued while mpv was already playing —
                            // it was already `false`, and stays `false`, with
                            // no change event through it — so the
                            // confirmation `played_since_play` waits for
                            // went silent from the second track onward were
                            // it still read from here.
                            None
                        }
                        // Established (see `OBSERVED` below): both
                        // `playlist-pos` and `chapter` are observed
                        // unconditionally, every session, regardless of
                        // what is loaded — not one or the other depending
                        // on the source. Folding both into a single event
                        // rests on an assumption that is **not measured on
                        // real hardware**: that for a playlist-based load
                        // (the files plugin's m3u) only `playlist-pos`
                        // actually keeps changing while `chapter` stays put
                        // after its own first report, and conversely that
                        // for a whole-disc `cdda://` load (the cd, one
                        // playlist entry) only `chapter` keeps changing
                        // while `playlist-pos` stays put. If that
                        // assumption is wrong, the core would relay a bogus
                        // track number it has no way to tell from a real
                        // one — the Source is not told which property
                        // produced it.
                        //
                        // The core relays the same event in both cases
                        // regardless, since only the Source knows what
                        // "track n" means for what it is playing. A
                        // negative value is passed through as-is and
                        // discarded by the Source, on the working — and
                        // itself unmeasured, not sourced to a specific
                        // manual section — assumption that `-1` is what
                        // `chapter` reads on a file with none. Plausible
                        // readings not ruled out: the property could
                        // instead be absent from mpv's replies for such a
                        // file (nothing to fold in this arm at all), or
                        // `-1` could mean "before the first chapter" on a
                        // file that does have some.
                        //
                        // Also not measured, and worth measuring
                        // separately: `playlist-pos` is documented to
                        // reflect whatever entry mpv currently considers
                        // current, which may flip before that entry's
                        // demuxer has actually opened. A `chapter` seek
                        // issued in that window (see the cd plugin's
                        // `pending_chapter`) could be refused by mpv; the
                        // error would only reach `debug` upstream, and the
                        // seek would be lost with no retry (see
                        // `player_track` in the cd plugin).
                        (Some("playlist-pos") | Some("chapter"), Value::Number(n)) => {
                            n.as_i64().map(Event::TrackChanged)
                        }
                        _ => None,
                        }
                    };
                    if let Some(ev) = ev {
                        // `mpsc` with no loss: a full channel means
                        // backpressure on this pump (the socket's readback
                        // waits), never a dropped event. A vanished
                        // receiver means the core loop is done, nobody
                        // left to serve.
                        if events.send(ev).await.is_err() {
                            break 'pump;
                        }
                    }
                }
            }
            tracing::warn!("mpv socket closed");
        });
        ipc
    }

    pub async fn command(&self, args: &[Value]) -> Result<Value> {
        self.command_named(json!(args)).await
    }

    /// Sends one command in any form mpv takes — the positional array
    /// (`command`) or the named object (`{"name": …, …}`), the only one that
    /// can pass `loadfile` its options without knowing which mpv version
    /// counts its positional arguments how — and waits for its response.
    pub async fn command_named(&self, cmd: Value) -> Result<Value> {
        let (id, rx) = self.send(cmd).await?;
        self.wait(id, rx, COMMAND_TIMEOUT).await
    }

    /// `command`, its response awaited until `deadline` at most (a lock
    /// chime's budget, `PAUSED_CHIME_START_BUDGET`…). The command is written
    /// whatever the time left — at zero, it is written and not awaited — so
    /// an mpv that answers late still applies it, in order with the others.
    /// A response that missed the deadline is `CommandTimeout`.
    pub async fn command_until(&self, args: &[Value], deadline: tokio::time::Instant) -> Result<Value> {
        let (id, rx) = self.send(json!(args)).await?;
        self.wait(id, rx, deadline.saturating_duration_since(tokio::time::Instant::now())).await
    }

    /// Writes one command, its response to come through `rx`. Only the
    /// wait is ever bounded, never the write: a write cut halfway would
    /// leave half a line in mpv's input, and every command after it lost.
    async fn send(&self, cmd: Value) -> Result<(u64, oneshot::Receiver<Result<Value>>)> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "command": cmd, "request_id": id });
        let mut w = self.writer.lock().await;
        if let Err(e) = w.write_all(format!("{msg}\n").as_bytes()).await {
            self.pending.lock().await.remove(&id);
            return Err(e.into());
        }
        Ok((id, rx))
    }

    /// Awaits the response to command `id` for `timeout` at most; past it
    /// the entry is dropped (a late response then answers nothing).
    async fn wait(&self, id: u64, rx: oneshot::Receiver<Result<Value>>, timeout: std::time::Duration) -> Result<Value> {
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => bail!("mpv: response abandoned"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(CommandTimeout.into())
            }
        }
    }

    pub async fn observe(&self, name: &str) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.command(&[json!("observe_property"), json!(id), json!(name)]).await?;
        Ok(())
    }
}

/// How long a command waits for mpv's response, unless it has a budget of
/// its own (`MpvIpc::command_until`).
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// mpv did not answer a command in the time it was given. Typed, so that
/// the end of a paused chime can tell a restore seek that mpv may still
/// apply (written, unanswered) from one it refused.
#[derive(Debug)]
pub struct CommandTimeout;

impl std::fmt::Display for CommandTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mpv: command timeout")
    }
}

impl std::error::Error for CommandTimeout {}

/// What a change of mpv's `paused-for-cache` means for the journal.
#[derive(Debug, PartialEq)]
enum StallNote {
    Began,
    Ended(std::time::Duration),
}

/// Follows mpv's `paused-for-cache`, so that a silence on the speakers
/// leaves a dated line with its duration in the journal.
///
/// Born from a cut of nearly a minute on Oüi FM (2026-10-10) that left no
/// trace at all: mpv runs with `--no-terminal`, and the core did not watch
/// this property. The cause could only be rebuilt afterwards by comparing
/// the bytes the TCP connection had received with the playback position —
/// one connection, never reopened, every byte received played, about 57 s
/// during which the server sent nothing. It was not possible to tell one
/// long cut from several short ones, nor to say when it happened.
///
/// Measured on mpv 0.37 against a server that goes silent without closing:
/// `true` 0.25 s into the silence, `false` 1.25 s after data resumes (the
/// read-ahead refilling). When the server closes during the silence, it is
/// `false` too, followed by an `end-file`. When idle, the property is
/// unavailable and the change carries no `data` at all — so anything other
/// than `true` ends a stall in progress, a station change or a stop
/// included.
///
/// The journal only, no `Event`: nothing in the core acts on it, and an
/// event would hand the loop a state to keep consistent for the sake of a
/// log line.
#[derive(Debug, Default)]
struct Stall {
    since: Option<std::time::Instant>,
}

impl Stall {
    fn change(&mut self, data: &Value, now: std::time::Instant) -> Option<StallNote> {
        match (data, self.since) {
            (Value::Bool(true), None) => {
                self.since = Some(now);
                Some(StallNote::Began)
            }
            (Value::Bool(true), Some(_)) => None,
            (_, Some(since)) => {
                self.since = None;
                Some(StallNote::Ended(now.saturating_duration_since(since)))
            }
            (_, None) => None,
        }
    }
}

/// The two observed properties that become `Event::TrackChanged`: the only
/// ones whose swallowed value is replayed (see `AsideGate`).
const TRACK_PROPERTIES: [&str; 2] = ["playlist-pos", "chapter"];

/// Keeps every event of a `play_aside` load (a lock chime played while idle)
/// away from the core, which must go on believing nothing plays.
///
/// It sees every line that is not a response. A `start-file` while armed is
/// the aside's own: it is swallowed and opens the gate, which then swallows
/// everything until the next `start-file` (a real load replacing the chime:
/// admitted) or an `idle-active: true` (the chime done: swallowed too, since
/// the core never saw it leave idle). Measured on mpv 0.37 by the lock-sound
/// spike (2026-11-16): a load sent right after the chime's response may come
/// before or after the chime's `start-file`, but the chime always gets its
/// own `start-file` first and ends with an `end-file`, with no idle between
/// it and the real file.
///
/// **mpv notifies a property only when its value changes**, so a swallowed
/// notification leaves mpv and the core disagreeing on that property; the
/// gate remembers each one (`pending`) until the disagreement is settled:
///
/// - **After the gate closes, mpv sets back what the chime moved.** Right
///   after `idle-active: true` come `playlist-pos: -1`, `path` and
///   `paused-for-cache` without data (measured on mpv 0.37 for this module's
///   tests, `IDLE_CHIME_TAIL`; the spike's fixture 1 stopped at
///   `idle-active: true` and so suggested they never came). They restore
///   what the core already holds: a notification equal to what the core was
///   last given for that property, after a swallowed one, is swallowed too —
///   exactly what mpv would not have sent had the chime never played.
/// - **A real load during the chime: swallowed track numbers are replayed.**
///   In 4 of the spike's 20 interrupted runs the single coalesced
///   `playlist-pos: 0` fell inside the chime although it was the real file's
///   value too, and mpv never sends it again. So right after the next
///   admitted `start-file`, the last swallowed `playlist-pos` and `chapter`
///   are handed back (`take_replay`) when they differ from what the core
///   holds; a real notification that follows wins, being later. Should mpv
///   ever leave a chime's track number in place after idle (what the spike's
///   truncated capture suggested: then a later real load at position 0 would
///   never be notified), the same replay covers the next real load.
/// - The other swallowed values are only adopted at that `start-file`, never
///   replayed: `path` and `media-title` are the chime's own `av://lavfi:…`
///   strings, which no real file shares, so the real load notifies its own;
///   the chime's `metadata` is `{}`, from which the core makes nothing; an
///   `idle-active: false` replaced by a real load stays false with no new
///   notification, and adopting it is what lets the later real
///   `idle-active: true` (a stop) through rather than mistake it for a
///   restoration. `paused-for-cache` only feeds the journal, which the pump
///   writes before the gate.
///
/// **It also keeps a paused chime from the core** (`sound_while_paused`):
/// mpv is unpaused for the chime and paused again, the file put back where
/// it stood, and the core must go on seeing a paused file. Measured by the
/// paused spike on mpv 0.37 (`PAUSED_CHIME` in the tests): of the lines that
/// sequence produces, `audio-reconfig` and `seek`, which the pump never
/// turns into events, and `playback-restart` — the flush seek's, during the
/// chime, and the restore seek's, which mpv sends only once the user
/// resumes, 0.29 to 0.4 s later. So every `playback-restart` is swallowed
/// during the chime, and after it exactly one more, whenever it comes,
/// unless a `start-file` comes first (a new file: nothing of the chime is
/// pending any more, and its own `playback-restart` is real). No
/// `start-file`, `end-file`, `idle` or observed property change in any run
/// (a file near its end does produce them: `ChimeRoute::Silent` there). A
/// `start-file` during the chime is the user's, admitted, and ends the
/// chime for the gate. mpv may also send the restore's `playback-restart`
/// while still paused (seen with `ao=null` on a loaded machine, about one
/// run in eight): it is swallowed then just the same. Were it ever lost,
/// the one swallowed in its place would be a later seek's, which the core
/// only reads to know the chain is live: it already was.
#[derive(Debug, Default)]
pub(crate) struct AsideGate {
    /// Asides sent whose `start-file` has not come yet.
    armed: u32,
    /// Between an aside's `start-file` and its end.
    inside: bool,
    /// A real load (`play`, `load_list`) sent whose `start-file` has not
    /// come yet: mpv is about to replace whatever sounds (`sounding_aside`).
    loading: bool,
    /// Per observed property, the value the core was last given.
    seen: HashMap<String, Value>,
    /// Per observed property, the last line swallowed since.
    pending: HashMap<String, Value>,
    /// Lines to hand to the core right after the admitted line.
    replay: Vec<Value>,
    /// A paused chime sounds (`sound_while_paused` until
    /// `end_sound_while_paused`): every `playback-restart` is its own.
    paused_chime: bool,
    /// The restore seek of a paused chime was sent and its
    /// `playback-restart` has not come yet (it comes once the user resumes).
    restore_pending: bool,
    /// What `progress` answers while a paused chime sounds, and after a
    /// restore until the user's next move (`end_paused_chime`, `thaw`) or a
    /// `start-file`: what it answered at the pause.
    frozen: Option<Progress>,
}

/// A property-change's value; absent (property unavailable) reads as `null`.
fn data_of(line: &Value) -> Value {
    line.get("data").cloned().unwrap_or(Value::Null)
}

impl AsideGate {
    /// An aside is about to be sent: its `start-file` is to be swallowed.
    pub(crate) fn arm(&mut self) {
        self.armed += 1;
    }

    /// The aside was not sent after all (its command failed).
    pub(crate) fn disarm(&mut self) {
        self.armed = self.armed.saturating_sub(1);
    }

    /// A real load is about to be sent (or, `false`, was refused).
    pub(crate) fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// Whether what mpv plays right now is an aside and nothing real is on
    /// its way to replace it: a new chime may then simply replace it
    /// (`chime_route`). `inside`, not `armed`: `armed` counts a `start-file`
    /// still to come, which can stay counted if it never comes, whereas
    /// `inside` is closed by any admitted `start-file` and by the chime's
    /// idle. `loading` covers the few milliseconds between a real `loadfile`
    /// and its `start-file`, while mpv still plays the chime it is about to
    /// replace: a chime sent then as an aside would replace the real file.
    pub(crate) fn sounding_aside(&self) -> bool {
        self.inside && !self.loading
    }

    /// Whether a real load was sent whose `start-file` has not come yet:
    /// nothing may replace it (`chime_route`).
    pub(crate) fn loading(&self) -> bool {
        self.loading
    }

    /// A paused chime is about to be written: `progress` is what the core
    /// reads until it ends. A second chime keeps the first one's.
    pub(crate) fn begin_paused_chime(&mut self, progress: Progress) {
        self.paused_chime = true;
        self.frozen.get_or_insert(progress);
    }

    /// Whether a paused chime sounds (`begin_paused_chime` until
    /// `end_paused_chime`, or a `start-file`).
    pub(crate) fn in_paused_chime(&self) -> bool {
        self.paused_chime
    }

    /// The progress frozen by a paused chime, if one sounds.
    pub(crate) fn frozen(&self) -> Option<Progress> {
        self.frozen
    }

    /// The paused chime is over; `restore_sent` when its restore seek was
    /// sent, whose `playback-restart` is still to come. Then `progress`
    /// stays frozen until the user moves playback (`thaw`) or a file starts,
    /// not until that `playback-restart`: after the restore, mpv's
    /// `time-pos` read while still paused said 0.17 s short of where the
    /// file was put back in about one run in four (mpv 0.37, `ao=null`, the
    /// test machine loaded), the restore's `playback-restart` having come
    /// during the pause in some of those runs, while playback did resume at
    /// the restored point — a bar going back for nothing. Nothing but the
    /// user moves a paused file.
    pub(crate) fn end_paused_chime(&mut self, restore_sent: bool) {
        self.paused_chime = false;
        self.restore_pending = restore_sent;
        if !restore_sent {
            self.frozen = None;
        }
    }

    /// The user moves playback (resume, seek, stop, another track): from
    /// now on `progress` is mpv's again, unless a paused chime sounds.
    pub(crate) fn thaw(&mut self) {
        if !self.paused_chime {
            self.frozen = None;
        }
    }

    /// Whether `line` (an event, never a response) reaches the core. When it
    /// does, `take_replay` gives what must follow it.
    pub(crate) fn admit(&mut self, line: &Value) -> bool {
        if line["event"] == json!("start-file") {
            self.paused_chime = false;
            self.restore_pending = false;
            self.frozen = None;
        } else if line["event"] == json!("playback-restart") && (self.paused_chime || self.restore_pending) {
            if !self.paused_chime {
                self.restore_pending = false;
            }
            return false;
        }
        if line["event"] == json!("start-file") {
            if self.armed > 0 {
                self.armed -= 1;
                self.inside = true;
                return false;
            }
            self.inside = false;
            self.loading = false;
            // Track numbers in a fixed order; the rest only adopted.
            for name in TRACK_PROPERTIES {
                if let Some(line) = self.pending.remove(name) {
                    let data = data_of(&line);
                    if self.seen.get(name) != Some(&data) {
                        self.seen.insert(name.to_string(), data);
                        self.replay.push(line);
                    }
                }
            }
            for (name, line) in self.pending.drain() {
                self.seen.insert(name, data_of(&line));
            }
            return true;
        }
        let property = (line["event"] == json!("property-change")).then(|| line["name"].as_str()).flatten();
        if self.inside {
            if property == Some("idle-active") && line["data"] == json!(true) {
                self.inside = false;
            }
            if let Some(name) = property {
                self.pending.insert(name.to_string(), line.clone());
            }
            return false;
        }
        if let Some(name) = property {
            let data = data_of(line);
            // mpv restoring, after the chime, what the core already holds.
            if self.pending.remove(name).is_some() && self.seen.get(name) == Some(&data) {
                return false;
            }
            self.seen.insert(name.to_string(), data);
        }
        true
    }

    /// The lines to process right after the line just admitted.
    pub(crate) fn take_replay(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.replay)
    }
}

/// Extracts the title the stream announces from the content of mpv's
/// `metadata` property. Pure function, testable on a real capture.
///
/// The key is looked up **case-insensitively**: mpv copies field names
/// exactly as the station sends them, and the ICY header shows up,
/// depending on the server, as `icy-title`, `Icy-Title` or `ICY-TITLE`.
///
/// An empty or blank value gives `None`, hence no event: several stations
/// measured emit an empty `StreamTitle` between two tracks (and OUI FM puts
/// filler text there). Clearing the display on every gap would make the
/// line flicker, while a track change already resets the slate on the core
/// side.
pub fn icy_title(data: &Value) -> Option<String> {
    let map = data.as_object()?;
    let raw = map
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("icy-title"))
        .and_then(|(_, value)| value.as_str())?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Extracts the three displayable fields from the **tags of the played
/// file**, from that same `metadata` property. Pure function, testable on a
/// real capture.
///
/// FFmpeg **normalizes** the keys: ID3 (mp3), Vorbis comments (flac, ogg,
/// opus), iTunes atoms (m4a) and RIFF INFO (wav) all surface under
/// `title` / `artist` / `album`, which was verified format by format. A
/// single grammar is therefore enough, and it covers the whole library.
///
/// Two precautions, both born from a measurement:
///
/// - only **three named keys are picked out** instead of absorbing the
///   object: an m4a also surfaces `major_brand`, `handler_name`,
///   `vendor_id` and `compatible_brands`, which have no place in a
///   display;
/// - the presence of an `icy-*` key **marks a stream** and returns `None`.
///   Some stations fill in a `title` equal to their own name alongside an
///   `icy-title` that carries the real track: preferring the former would
///   be a silent regression for the radio.
pub fn file_tags(data: &Value) -> Option<Track> {
    let map = data.as_object()?;
    if map.keys().any(|key| key.to_ascii_lowercase().starts_with("icy-")) {
        return None;
    }
    let field = |name: &str| {
        map.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let track = Track {
        artist: field("artist"),
        title: field("title"),
        album: field("album"),
        duration_s: None,
        // FFmpeg normalizes the year under `date`, whatever the file
        // format — as it already does for the three fields above. Its
        // value ranges from `"1959"` to `"1959-08-17"` depending on what
        // the tag carries, hence the pass through `valid_year`.
        year: field("date").as_deref().and_then(ritornello_proto::valid_year),
        // A local file carries no platform link: only streams announce
        // those.
        links: Vec::new(),
        origin: Some(crate::metadata::ORIGIN_TAGS.to_string()),
        cover_href: None,
        cover_origin: None,
        // Like `cover_href`: a local URL the core builds once it holds
        // lyrics, never something a tag carries.
        lyrics_href: None,
        lyrics_synced: false,
        lyrics_origin: None,
        // Left empty here: it is `Metadata::text_block` that attributes
        // these fields to the tags, at composition time. Filling it in
        // here too would give two truths to keep in sync.
        provenance: Default::default(),
    };
    (!track.is_empty()).then_some(track)
}

/// Lyrics embedded in the **tags of the played file**, from that same
/// `metadata` property, validated through `Lyrics::validated`.
///
/// The keys, **measured** on 2026-11-03 (ffmpeg 6.1.1, mpv 0.37.0, the
/// `metadata` property read by a script on `file-loaded`, cross-checked
/// with ffprobe's `format_tags`):
///
/// - mp3, a real ID3v2 `USLT` frame (language `eng`): `lyrics-eng`; with a
///   content descriptor `desc`: `lyrics-desc-eng`. FFmpeg names the frame
///   `lyrics-[descriptor-]language`, so the key is never just `lyrics`;
/// - mp3 tagged by ffmpeg itself (`-metadata lyrics=…`): ffmpeg writes a
///   `TXXX:USLT` frame, not a `USLT`, and reads it back as `lyrics`;
/// - flac, Vorbis comment `LYRICS`: `LYRICS` (case kept);
/// - flac, Vorbis comment `UNSYNCEDLYRICS` (foobar2000, Mp3tag): kept as
///   `UNSYNCEDLYRICS`;
/// - m4a, iTunes atom `©lyr`: `lyrics`.
///
/// Hence the grammar, without regard to case: `lyrics`, else the first key
/// starting with `lyrics-`, as the words — LRC if they parse, plain text
/// otherwise, `validated` deciding — and `unsyncedlyrics` as the plain
/// text beside them.
///
/// Silent on a stream (an `icy-*` key present), for `file_tags`' reason: what
/// a station puts beside its ICY title does not describe the song.
pub fn file_lyrics(data: &Value) -> Option<Lyrics> {
    let map = data.as_object()?;
    if map.keys().any(|key| key.to_ascii_lowercase().starts_with("icy-")) {
        return None;
    }
    let text = |pick: &dyn Fn(&str) -> bool| {
        map.iter()
            .find(|(key, _)| pick(&key.to_ascii_lowercase()))
            .and_then(|(_, value)| value.as_str())
            .map(str::to_string)
    };
    let words = text(&|k| k == "lyrics").or_else(|| text(&|k| k.starts_with("lyrics-")));
    let plain = text(&|k| k == "unsyncedlyrics");
    Lyrics { plain, synced: words }.validated()
}

/// The `FileTags` event a `metadata` property yields: emitted when the tags
/// carry text **or** lyrics, so a file whose only tag is its words is not
/// lost, and nothing for a stream (both readers are silent on `icy-*`).
pub fn file_tags_event(data: &Value) -> Option<Event> {
    let track = file_tags(data).map(Box::new);
    let lyrics = file_lyrics(data);
    (track.is_some() || lyrics.is_some()).then_some(Event::FileTags { track, lyrics })
}

/// What the core reads, itself, from a local file a Source declared
/// (`SourceMessage::media_path` while it plays, `Armed::media_path` while it
/// is armed): its tags, its embedded lyrics and its embedded cover. See
/// `read_media_file`.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFile {
    pub tags: Option<Track>,
    pub lyrics: Option<Lyrics>,
    pub cover: Option<crate::cover::CoverSource>,
}

/// Every tag `file` carries, the format's primary one first. FFmpeg merges
/// the tags a file carries (an mp3's ID3v2 and APE, a flac's Vorbis comments
/// and a stray ID3), so a field is looked up in each, in this order, and the
/// first that has it wins — for the cover, the text and the lyrics alike.
fn tags_of(file: &lofty::file::TaggedFile) -> Vec<&lofty::tag::Tag> {
    use lofty::file::TaggedFileExt;
    let primary = file.primary_tag();
    let primary_type = primary.map(lofty::tag::Tag::tag_type);
    primary.into_iter().chain(file.tags().iter().filter(|t| Some(t.tag_type()) != primary_type)).collect()
}

/// The words an mp3's ID3v2 tag carries in `TXXX` frames, which `lofty`
/// keeps out of its generic tag (it maps no `TXXX` description to its
/// lyrics keys but `USLT`): `(words, plain)`, from `TXXX:LYRICS` and
/// `TXXX:UNSYNCEDLYRICS`, descriptions compared without regard to case, as
/// FFmpeg exposes them as `LYRICS` and `UNSYNCEDLYRICS` and `file_lyrics`
/// reads them. Some taggers write them there.
fn id3_user_lyrics(tag: &lofty::id3::v2::Id3v2Tag) -> (Option<String>, Option<String>) {
    let user = |name: &str| {
        tag.into_iter().find_map(|frame| match frame {
            lofty::id3::v2::Frame::UserText(f) if f.description.eq_ignore_ascii_case(name) => {
                Some(f.content.to_string())
            }
            _ => None,
        })
    };
    (user("lyrics"), user("unsyncedlyrics"))
}

/// The embedded picture, as a cover the core can serve.
///
/// **Symmetrical with a Source's `folder.jpg`, and that symmetry is the whole
/// design.** A local cover — wherever it lives — costs the core only a path:
/// `CoverPayload::File` re-reads a `folder.jpg` from its path on every
/// request, and `CoverPayload::Embedded` does the same for a picture inside
/// the audio file, re-opening it through `read_embedded_bounded` rather than
/// keeping a copy anywhere. Nothing here loads the picture's bytes into a
/// `CoverPayload`, and nothing here writes to the disk: the only output is
/// `cover::CoverSource::Embedded { audio, content }`, the audio file's own
/// path plus a fingerprint of the picture's bytes.
///
/// `content` (see `cover::content_key`) is what a fifteen-track album needs
/// to collapse onto a single cache entry: two tracks sharing one embedded
/// picture yield two different `audio` paths but the identical `content`,
/// and `cover::key` hashes only the latter — see its doc. Computing it still
/// costs one `lofty` read per track, that part is irreducible: the bytes must
/// be in hand to be hashed, whether or not they end up written anywhere.
fn cover_of(path: &str, tags: &[&lofty::tag::Tag]) -> Option<crate::cover::CoverSource> {
    let image = tags.iter().find_map(|t| t.pictures().first())?;
    Some(crate::cover::CoverSource::Embedded {
        audio: std::path::PathBuf::from(path),
        content: crate::cover::content_key(image.data()),
    })
}

/// The lyrics embedded in `tag`, read with the grammar `file_lyrics` applies
/// to what FFmpeg exposes of the same tags, so a file's words read the same
/// as they did when mpv was the reader: the words — LRC if they parse, plain
/// text otherwise, `Lyrics::validated` deciding — and, beside them, text a
/// tagger marked as unsynchronised.
///
/// How `lofty` names what `file_lyrics` reads, format by format (lofty
/// 0.25, its `ItemKey` mappings):
///
/// - mp3: a real ID3v2 `USLT` frame and ffmpeg's own `TXXX` frame described
///   `USLT` (`-metadata lyrics=…`, see `test_mp3_with_lyrics`) both become
///   `UnsyncLyrics`. FFmpeg exposes them as `lyrics-<lang>` and `lyrics`,
///   which `file_lyrics` reads as the words, so `UnsyncLyrics` is the words
///   here (lofty has no `Lyrics` for ID3v2);
/// - flac/ogg/opus: the Vorbis comment `LYRICS` is `Lyrics` (the words),
///   `UNSYNCEDLYRICS` is `UnsyncLyrics`, the plain text beside them, as
///   `file_lyrics` reads `unsyncedlyrics`;
/// - m4a: the iTunes atom `©lyr` is `Lyrics` (the words).
///
/// `id3_user` is what `id3_user_lyrics` found in an mp3's `TXXX` frames:
/// FFmpeg exposes `TXXX:LYRICS` as `lyrics`, ahead of a `USLT` frame's
/// `lyrics-<lang>` in `file_lyrics`' precedence, so it comes first here too.
fn lyrics_of(tag: &lofty::tag::Tag, id3_user: &(Option<String>, Option<String>)) -> Option<Lyrics> {
    use lofty::tag::{ItemKey, TagType};
    let text = |key: ItemKey| tag.get_string(key).map(str::to_string);
    let (words, plain) = if tag.tag_type() == TagType::Id3v2 {
        let (user_words, user_plain) = id3_user.clone();
        (user_words.or_else(|| text(ItemKey::Lyrics)).or_else(|| text(ItemKey::UnsyncLyrics)), user_plain)
    } else {
        (text(ItemKey::Lyrics), text(ItemKey::UnsyncLyrics))
    };
    Lyrics { plain, synced: words }.validated()
}

/// Tags, embedded lyrics and embedded cover of a local file, read with
/// `lofty` in a single parse of the container: on a share, every open is the
/// expensive part. **Strictly blocking**, potentially on a network share
/// that never answers: only under `Health::bounded`, never directly from an
/// async task — see `Core::spawn_file_read` and `health.rs` for why. `None`
/// when the path has a scheme or the file cannot be read.
///
/// The one reader of a file a Source declares, armed or playing. mpv's own
/// announcements no longer describe a local file: they travel on another
/// channel than the Source's, and could arrive before the Source said which
/// track plays — the race that left a new track without its cover. The text
/// follows what `file_tags` reads from mpv for a stream: the same three
/// fields, trimmed, empty → `None`, the year through the same `valid_year`,
/// the same `ORIGIN_TAGS`.
pub fn read_media_file(path: &str) -> Option<MediaFile> {
    use lofty::file::AudioFile;
    use lofty::tag::Accessor;
    if path.contains("://") {
        return None;
    }
    // Only a regular file is opened. A FIFO (or a device, a socket) blocks
    // the open itself until a writer shows up, which no deadline here can
    // cut: `Health::bounded` would time out and mark the **whole mount**
    // unreachable for a path that merely names something unreadable.
    // `metadata` follows a symlink, so a link to a real file still reads.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    // An mp3 is read as such, still in one parse: its ID3v2 tag keeps the
    // `TXXX` frames lofty's generic tag leaves out (`id3_user_lyrics`).
    let probe = lofty::probe::Probe::open(path).ok()?.guess_file_type().ok()?;
    let (file, id3_user) = if probe.file_type() == Some(lofty::file::FileType::Mpeg) {
        use std::io::Seek;
        let mut reader = probe.into_inner();
        reader.rewind().ok()?;
        let mpeg = lofty::mpeg::MpegFile::read_from(&mut reader, lofty::config::ParseOptions::new()).ok()?;
        let user = mpeg.id3v2().map(id3_user_lyrics).unwrap_or_default();
        (lofty::file::TaggedFile::from(mpeg), user)
    } else {
        (probe.read().ok()?, (None, None))
    };
    let tags = tags_of(&file);
    let clean = |value: Option<std::borrow::Cow<'_, str>>| {
        value.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    // The first tag that has the field, as FFmpeg's merged view gives it.
    fn first<T>(tags: &[&lofty::tag::Tag], read: impl Fn(&lofty::tag::Tag) -> Option<T>) -> Option<T> {
        tags.iter().find_map(|t| read(t))
    }
    // Truncated, as the core truncates mpv's measured duration
    // (`refresh_position`): rounding would show 3:34 armed for a track
    // that shows 3:33 once it plays.
    let seconds = u32::try_from(file.properties().duration().as_secs()).unwrap_or(u32::MAX);
    let track = Track {
        artist: first(&tags, |t| clean(t.artist())),
        title: first(&tags, |t| clean(t.title())),
        album: first(&tags, |t| clean(t.album())),
        duration_s: (seconds > 0).then_some(seconds),
        // The raw text, as FFmpeg hands `date` to `file_tags`: the same
        // `valid_year` then judges `"1971"` and `"1971-08-17"` alike.
        year: first(&tags, |t| {
            t.get_string(lofty::tag::ItemKey::RecordingDate)
                .or_else(|| t.get_string(lofty::tag::ItemKey::Year))
                .and_then(ritornello_proto::valid_year)
        }),
        links: Vec::new(),
        origin: Some(crate::metadata::ORIGIN_TAGS.to_string()),
        cover_href: None,
        cover_origin: None,
        // Same as in `file_tags`: built by the core, not read from tags.
        lyrics_href: None,
        lyrics_synced: false,
        lyrics_origin: None,
        provenance: Default::default(),
    };
    Some(MediaFile {
        tags: (!track.is_empty()).then_some(track),
        lyrics: tags.iter().find_map(|t| lyrics_of(t, &id3_user)),
        cover: cover_of(path, &tags),
    })
}

pub struct MpvPlayer {
    ipc: Arc<MpvIpc>,
    /// Where the lock chime files were written at start (`lock_sound::DIR_NAME`
    /// under the runtime directory, beside the socket).
    chime_dir: std::path::PathBuf,
}

impl MpvPlayer {
    fn gate(&self) -> std::sync::MutexGuard<'_, AsideGate> {
        self.ipc.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A load the core sees (`loadfile`, `loadlist`): the gate learns of it
    /// **before** it is written, since its `start-file` can arrive before
    /// its response (`AsideGate::sounding_aside`).
    async fn real_load(&self, cmd: &[Value]) -> Result<Value> {
        self.gate().thaw();
        self.gate().set_loading(true);
        let result = self.ipc.command(cmd).await;
        if result.is_err() {
            self.gate().set_loading(false);
        }
        result
    }

    fn get(&self, name: &str) -> [Value; 2] {
        [json!("get_property"), json!(name)]
    }

    /// `get_property name`, answered by `deadline` — or not sent at all once
    /// it has passed: a question nobody waits for is worth nothing.
    async fn ask_until(&self, name: &str, deadline: tokio::time::Instant) -> Result<Value> {
        if tokio::time::Instant::now() >= deadline {
            return Err(CommandTimeout.into());
        }
        self.ipc.command_until(&self.get(name), deadline).await
    }

    /// A paused chime's command — the chain, a seek, the pause — answered by
    /// `deadline`, or not sent once it has passed (see `ask_until`).
    async fn act_until(&self, args: &[Value], deadline: tokio::time::Instant) -> Result<Value> {
        if tokio::time::Instant::now() >= deadline {
            return Err(CommandTimeout.into());
        }
        self.ipc.command_until(args, deadline).await
    }

    /// `chime_route`'s questions to mpv, in the paused spike's order.
    /// The questions share `CHIME_ROUTE_BUDGET`: one that misses it fails
    /// (`CommandTimeout`), and so does every one after, unsent. A late
    /// answer to `time-pos` or `duration` would otherwise read as "none"
    /// and drop the near-end guard: once the budget is spent, `seekable`
    /// fails too, and the route is `Silent`.
    async fn ask_chime_route(&self, chime: std::time::Duration) -> Result<super::ChimeRoute> {
        use super::ChimeRoute;
        let deadline = tokio::time::Instant::now() + CHIME_ROUTE_BUDGET;
        if self.ask_until("idle-active", deadline).await? == json!(true) {
            return Ok(ChimeRoute::Idle);
        }
        let paused = self.ask_until("pause", deadline).await? == json!(true);
        let params = self.ask_until("audio-params", deadline).await.unwrap_or(Value::Null);
        let field = |name: &str| params[name].as_u64().and_then(|n| u32::try_from(n).ok());
        let (Some(sample_rate), Some(channels)) = (field("samplerate"), field("channel-count")) else {
            return Ok(ChimeRoute::Silent);
        };
        if !paused {
            return Ok(ChimeRoute::Live { sample_rate, channels });
        }
        let position = number_or_none(self.ask_until("time-pos", deadline).await);
        let duration = number_or_none(self.ask_until("duration", deadline).await);
        // Only an explicit `false` says "a stream": an unavailable `seekable`
        // (an error) or anything but a boolean says nothing, and taking a
        // file for a stream would leave it unrestored and unguarded against
        // its end. No chime then.
        let Ok(Value::Bool(seekable)) = self.ask_until("seekable", deadline).await else {
            return Ok(ChimeRoute::Silent);
        };
        if !seekable {
            // A stream's `duration` is its cache end (measured): no guard.
            return Ok(ChimeRoute::Paused { sample_rate, channels, resume_at: None });
        }
        // After a restore, still paused, the pause point is the one the gate
        // kept: `time-pos` then read up to 0.17 s short (`end_paused_chime`),
        // and a second chime would put the file back there.
        let kept = self.gate().frozen().and_then(|p| p.position_s);
        let Some(position) = kept.or(position) else {
            // Nowhere to put the file back: the chime would move it.
            return Ok(ChimeRoute::Silent);
        };
        if !super::paused_chime_fits(position, duration, chime) {
            return Ok(ChimeRoute::Silent);
        }
        Ok(ChimeRoute::Paused { sample_rate, channels, resume_at: Some(position) })
    }
}

/// How long the end of a paused chime waits for the `audio-reconfig` of its
/// chain rewrite before the restore seek. Measured by the paused spike on
/// mpv 0.37: a restore seek sent before that rebuild was lost once in about
/// 49 (the audio resumed 2.7 s off while `time-pos` claimed the restored
/// position); with the wait, never in 50, and the wait itself took under
/// 0.1 ms each time. The guard is reasoned from that one failure, not proven.
const RESTORE_RECONFIG_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// The lock chime runs on the core loop, which also serves the remote and
/// the page: no part of it may wait out mpv's 5 s per command
/// (`COMMAND_TIMEOUT`), which made a lock press on an mpv that stopped
/// answering hold the loop for about 30 s (the route's six questions), 25 s
/// (the start's five commands) and 16 s (the end's three, plus the chain
/// rebuild's wait). Each step now has a total budget; mpv answers each
/// command in well under a millisecond on the dev machine, so a budget is
/// only ever spent by an mpv in trouble.
///
/// The route's questions, all of them.
const CHIME_ROUTE_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
/// The start of a paused chime (`sound_while_paused`): the pause's
/// progress, the chain, the flush seek, the unpause. A step past it is not
/// sent, the call fails, and the core ends the chime at once.
pub(crate) const PAUSED_CHIME_START_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);
/// The end of a paused chime (`end_sound_while_paused`): the pause, the
/// chain, the wait for its rebuild (`RESTORE_RECONFIG_WAIT`, cut to what is
/// left), the restore seek. Unlike the start, every step is written even
/// once the budget is spent, only not awaited: an mpv that answers late
/// applies them in order, and ends paused, on the plain chain, put back.
pub(crate) const PAUSED_CHIME_END_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// Brings a `get_property` response down to a usable number.
///
/// Three ways for mpv to say "I don't know", all brought down to `None`:
/// the error (`property unavailable` on a stream with no duration), the
/// `null`, and the negative value mpv briefly produces at a file's startup
/// — measured at `-0.02`, and publishing that would make the bar go
/// backwards.
fn number_or_none(res: Result<Value>) -> Option<f64> {
    res.ok().and_then(|v| v.as_f64()).filter(|n| *n >= 0.0)
}

/// Audio output buffer, in seconds. **We reuse mpv's default**, so this
/// module changes nothing about behavior as long as the variable is not
/// set: the cause of the observed micro-glitches is not established, and
/// widening it by default would have masked the diagnosis rather than
/// making it. The dial exists because the right value depends on the
/// machine — on a Pi 2, a load spike can make an ALSA write deadline be
/// missed, which is heard as a micro-glitch, and raising it to 0.5 s is
/// then the first thing to try. The cost is a matching latency on picking
/// up volume or mute changes, imperceptible for radio.
pub const AUDIO_BUFFER_DEFAULT: f64 = 0.2;

/// Upper bound mpv imposes on `--audio-buffer`.
const AUDIO_BUFFER_MAX: f64 = 10.0;

/// Playback read-ahead, in seconds. **We reuse mpv's default**, for the
/// same reason as the output buffer: change nothing without having
/// measured. One second is thin for an internet stream, though — the
/// slightest network jitter clears the read-ahead and mpv pauses playback
/// while it refills — so this is the dial to turn first on a flaky link.
/// Ten seconds of 128 kbit/s MP3 weighs about 160 KB, negligible even on 1
/// GB of RAM.
pub const READAHEAD_DEFAULT: f64 = 1.0;

/// Upper bound kept here: beyond it, the buffer costs memory with no
/// audible benefit, and delays picking up a station change.
const READAHEAD_MAX: f64 = 120.0;

/// Reads a duration supplied by the environment. Variable absent: the
/// default, silently. Value unreadable, negative or out of bounds: the
/// default **with** a warning, rather than a startup failure — a silent
/// device because a variable was mistyped would be a worse outcome than a
/// default setting.
fn duration_setting(raw: Option<&str>, default: f64, max: f64, what: &str) -> f64 {
    let Some(raw) = raw else { return default };
    match raw.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && (0.0..=max).contains(&v) => v,
        Ok(v) => {
            tracing::warn!("{what}={v} out of bounds (0..={max}), keeping {default}");
            default
        }
        Err(e) => {
            tracing::warn!("{what}={raw:?} unreadable ({e}), keeping {default}");
            default
        }
    }
}

/// Output buffer to keep, based on `RITORNELLO_AUDIO_BUFFER` if it is set.
pub fn audio_buffer_setting(raw: Option<&str>) -> f64 {
    duration_setting(raw, AUDIO_BUFFER_DEFAULT, AUDIO_BUFFER_MAX, "RITORNELLO_AUDIO_BUFFER")
}

/// Playback read-ahead to keep, based on `RITORNELLO_NETWORK_READAHEAD`.
pub fn readahead_setting(raw: Option<&str>) -> f64 {
    duration_setting(raw, READAHEAD_DEFAULT, READAHEAD_MAX, "RITORNELLO_NETWORK_READAHEAD")
}

/// mpv launch arguments. Pure function, separate from `start` to be
/// testable without spawning a process.
///
/// `af` is the audio filter chain (the equalizer's); an empty one adds no
/// `--af` at all.
pub fn mpv_args(socket: &Path, cd_dev: &str, audio_buffer: f64, readahead: f64, af: &str) -> Vec<String> {
    let mut args = vec![
        "--idle=yes".to_string(),
        "--no-video".to_string(),
        // `--no-video` does not stop mpv from opening a `cover.jpg` it finds
        // beside the track: it still loads it as an external track, at every
        // file, at a cost that grows with the image. Measured on the device
        // (mpv 0.40, Pi, SMB share): a 2.3 MB `cover.jpg` delayed every track
        // of its folder by 2.7 s, which this option removes; a 13 KB
        // `Folder.jpg` cost nothing measurable. The cover is the files
        // plugin's business, never mpv's.
        "--cover-art-auto=no".to_string(),
        "--no-terminal".to_string(),
        format!("--input-ipc-server={}", socket.display()),
        format!("--cdda-device={cd_dev}"),
        format!("--audio-buffer={audio_buffer}"),
        format!("--demuxer-readahead-secs={readahead}"),
        // mpv's yt-dlp hook stays loaded, and is neutral until a `Play`
        // names a program (`Player::set_ytdl`). `exclude=.` (a Lua pattern
        // matching any address) is meant to keep every `http(s)` address out
        // of the hook's fallback for good, so that a dead radio fails at once;
        // that is reasoned from mpv's source, NOT measured (every non-web load
        // ran `/bin/false`, which the counters could not see; nothing on
        // 0.40). What was measured, on mpv 0.37 only: a web-streaming load
        // always reaches the hook as `ytdl://`, which the exclusion does not
        // cover (100/100 with it, 25/100 with the old toggle). The program
        // starts as one that exists
        // and does nothing, so a `ytdl://` address loaded before any `Play`
        // runs no `yt-dlp` found on the `PATH`.
        format!("--script-opts=ytdl_hook-exclude=.,ytdl_hook-ytdl_path={YTDL_NOWHERE}"),
    ];
    if !af.is_empty() {
        args.push(format!("--af={af}"));
    }
    args
}

/// The program mpv's yt-dlp hook runs for a load that named none: it
/// exists, so the hook adopts it (a path that does not exist would leave the
/// hook running the program it found before), and it exits non-zero at once
/// without reading anything. See `Player::set_ytdl` on `MpvPlayer`.
pub const YTDL_NOWHERE: &str = "/bin/false";

/// Properties the core asks mpv to push. `metadata` carries the ICY header
/// received from the station (key `icy-title`), the only title source
/// available for a radio with no dedicated `metadata` plugin. `path` and
/// `paused-for-cache` only feed the journal (see `Stall`): which file or
/// stream stalled. mpv's `path` no longer tells the core which local file to
/// describe — the Source declares it with the identity
/// (`SourceMessage::media_path`), and the core never interprets the opaque
/// identity to derive one.
const OBSERVED: [&str; 7] = [
    "media-title",
    "metadata",
    "idle-active",
    "playlist-pos",
    "chapter",
    "path",
    "paused-for-cache",
];

/// Launches mpv as an idle daemon and connects to it. The Child is handed
/// back to the caller: if it dies, main exits and systemd restarts the
/// whole service.
pub async fn start(
    mpv_bin: &str,
    socket: &Path,
    cd_dev: &str,
    audio_buffer: f64,
    readahead: f64,
    af: &str,
    events: mpsc::Sender<Event>,
) -> Result<(MpvPlayer, tokio::process::Child)> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket);
    // mpv reads the lock chime from these files, which therefore exist
    // before it starts. A failure costs the chime, never the start.
    let chime_dir = socket.parent().unwrap_or_else(|| Path::new(".")).join(crate::lock_sound::DIR_NAME);
    if let Err(e) = crate::lock_sound::write_files(&chime_dir) {
        tracing::warn!("lock sound files not written to {}: {e}", chime_dir.display());
    }
    let child = tokio::process::Command::new(mpv_bin)
        .args(mpv_args(socket, cd_dev, audio_buffer, readahead, af))
        .kill_on_drop(true)
        .spawn()
        .context("starting mpv")?;

    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stream = stream.context("connecting to mpv socket (10 s)")?;
    let ipc = MpvIpc::from_stream(stream, events);
    for property in OBSERVED {
        ipc.observe(property).await?;
    }
    Ok((MpvPlayer { ipc, chime_dir }, child))
}

#[async_trait::async_trait]
impl super::Player for MpvPlayer {
    fn chime_dir(&self) -> &Path {
        &self.chime_dir
    }
    async fn play(&self, uri: &str) -> Result<()> {
        self.real_load(&[json!("loadfile"), json!(uri), json!("replace")]).await?;
        self.ipc.command(&[json!("set_property"), json!("pause"), json!(false)]).await?;
        Ok(())
    }
    /// `loadlist` and not `loadfile`: the list is unfolded **before** the
    /// command answers (its response even carries `num_entries`).
    ///
    /// With `loadfile`, measured on mpv 0.37: `playlist-count` is first 1,
    /// position 0, then an `end-file` and a `start-file` come before the
    /// count reaches 3. A requested position therefore arrived out of
    /// bounds, and the unfolding replayed the first track.
    ///
    /// **`playlist-start` before `loadlist`, and never a reposition after
    /// it.** Measured on mpv 0.37 with a three-entry list and entry 2
    /// wanted: loading then repositioning publishes `path` for entry 0
    /// **and then** for entry 2 — mpv really did open the first entry —
    /// whereas declaring the index beforehand only ever publishes entry 2.
    /// Everything the core hangs off `path` (the embedded-cover read, the
    /// identity relayed to the Source) therefore used to fire once on a
    /// track nobody had asked for.
    ///
    /// **Always sent, `auto` included.** The option is persistent, and this
    /// is measured too: a second `loadlist` sent without touching it starts
    /// again at the index the previous load declared. Omitting it for "from
    /// the beginning" would make a list loaded after a resume silently
    /// start on the resumed track. `auto` is mpv's own default, the same
    /// convention as the audio device.
    async fn load_list(&self, uri: &str, start: Option<i64>) -> Result<()> {
        let start = match start {
            Some(n) => json!(n),
            None => json!("auto"),
        };
        self.ipc.command(&[json!("set_property"), json!("playlist-start"), start]).await?;
        self.real_load(&[json!("loadlist"), json!(uri), json!("replace")]).await?;
        self.ipc.command(&[json!("set_property"), json!("pause"), json!(false)]).await?;
        Ok(())
    }
    async fn stop(&self) -> Result<()> {
        self.gate().thaw();
        self.ipc.command(&[json!("stop")]).await?;
        Ok(())
    }
    async fn toggle_pause(&self) -> Result<()> {
        self.gate().thaw();
        self.ipc.command(&[json!("cycle"), json!("pause")]).await?;
        Ok(())
    }
    async fn next(&self) -> Result<()> {
        self.gate().thaw();
        self.ipc.command(&[json!("playlist-next")]).await?;
        Ok(())
    }
    async fn prev(&self) -> Result<()> {
        self.gate().thaw();
        self.ipc.command(&[json!("playlist-prev")]).await?;
        Ok(())
    }
    async fn set_volume(&self, volume: u8) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("volume"), json!(volume)]).await?;
        Ok(())
    }
    async fn set_mute(&self, mute: bool) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("mute"), json!(mute)]).await?;
        Ok(())
    }
    async fn set_audio_device(&self, device: &str) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("audio-device"), json!(device)]).await?;
        Ok(())
    }
    async fn progress(&self) -> Result<Progress> {
        // While a paused chime sounds, `time-pos` moves with it and comes
        // back after (measured): the core reads the pause's.
        if let Some(frozen) = self.gate().frozen() {
            return Ok(frozen);
        }
        // Two round trips per second on a local Unix socket: the cost is
        // nil next to the interval. A poll rather than an
        // `observe_property` because mpv does not pace its `time-pos`
        // notifications — it would emit several per second for information
        // published once per second.
        //
        // Open question, not resolved: on a `cdda://` opened as a whole
        // disc, mpv exposes its tracks as chapters (see above, about track
        // advance). What does `time-pos` mean in that case — relative to
        // the disc or to the track? This is not measured on real hardware,
        // only noted in an archived design document. If the answer is
        // "relative to the disc", this value must subtract the start of
        // the current chapter, and `duration_s` should reflect the
        // chapter's rather than the whole disc's.
        let position = self.ipc.command(&[json!("get_property"), json!("time-pos")]).await;
        let duration = self.ipc.command(&[json!("get_property"), json!("duration")]).await;
        Ok(Progress { position_s: number_or_none(position), duration_s: number_or_none(duration) })
    }

    async fn seek_relative(&self, delta_s: i64) -> Result<()> {
        self.gate().thaw();
        self.ipc
            .command(&[json!("seek"), json!(delta_s), json!("relative")])
            .await
            .map(|_| ())
    }

    async fn seek_absolute(&self, position_s: u32) -> Result<()> {
        self.gate().thaw();
        self.ipc
            .command(&[json!("seek"), json!(position_s), json!("absolute")])
            .await
            .map(|_| ())
    }

    async fn set_chapter(&self, n: i64) -> Result<()> {
        self.gate().thaw();
        self.ipc.command(&[json!("set_property"), json!("chapter"), json!(n)]).await?;
        Ok(())
    }

    async fn set_loop_track(&self, on: bool) -> Result<()> {
        let value = if on { "inf" } else { "no" };
        self.ipc.command(&[json!("set_property"), json!("loop-file"), json!(value)]).await?;
        Ok(())
    }

    /// One `change-list script-opts append ytdl_hook-ytdl_path=…`, the
    /// source's program or [`YTDL_NOWHERE`], and nothing else: the hook
    /// script stays loaded for mpv's whole life.
    ///
    /// **Never `set_property ytdl`.** `ytdl` is a built-in-script option:
    /// `false` unloads `ytdl_hook.lua` and `true` loads it anew,
    /// asynchronously, so the `loadfile ytdl://…` sent right after found no
    /// hook yet and mpv answered "No protocol handler found". Measured on mpv
    /// 0.37 in the core's order with no delay (25 runs each,
    /// `tools/c1-race.py` of the design records): the toggle reached the
    /// wrapper in 25 of 100 web plays following another source's play (four
    /// series: 0, 10, 8 and 7 of 25); this one option in 100 of 100,
    /// playlist loads included.
    ///
    /// **A program that exists, not one that does not.** The hook remembers
    /// the last program it *found*, and runs it when a new path is not found:
    /// pointed at `/dev/null/yt-dlp`, a `ytdl://` address in another
    /// source's load still ran the previous web playback's wrapper once per
    /// run (25/25), on the hook's second attempt. [`YTDL_NOWHERE`] is found,
    /// so it replaces the remembered one. A dead `http` stream is meant never
    /// to reach the hook: [`mpv_args`] excludes every `http(s)` address from
    /// the hook's fallback for mpv's whole life. That effect is reasoned from
    /// mpv's source, not measured (on 0.37 or 0.40).
    ///
    /// The option is read by the next load: the hook script receives the
    /// change before that load's hook runs (measured on mpv 0.37 only, same
    /// runs). No
    /// `--ytdl-format` is needed: under `--no-video` mpv already asks for
    /// `bestaudio/best`.
    async fn set_ytdl(&self, path: Option<&std::path::Path>) -> Result<()> {
        let program = path.map_or_else(|| YTDL_NOWHERE.to_string(), |p| p.display().to_string());
        let option = format!("ytdl_hook-ytdl_path={program}");
        self.ipc
            .command(&[json!("change-list"), json!("script-opts"), json!("append"), json!(option)])
            .await?;
        Ok(())
    }
    async fn set_audio_filters(&self, chain: &str) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("af"), json!(chain)]).await?;
        Ok(())
    }
    async fn audio_filter_command(&self, cmd: &crate::equalizer::AfCommand) -> Result<()> {
        // The fifth element, the filter's name inside the lavfi graph, is
        // mandatory: measured on mpv 0.37, a missing target or `all` fails
        // with "error running command".
        self.ipc
            .command(&[json!("af-command"), json!(cmd.label), json!(cmd.command), json!(cmd.arg), json!(cmd.target)])
            .await?;
        Ok(())
    }

    /// Three questions in order, stopping at the first that settles it:
    /// `idle-active`, `pause`, `audio-params`. Audio parameters that are an
    /// error, a `null` or lack the rate or the channel count mean a file
    /// still loading: nothing to mix into yet.
    ///
    /// **An aside sounding is idle.** While an idle chime plays, mpv answers
    /// like a real file: `idle-active` false, `pause` false (the chime's
    /// file-local option), valid `audio-params` (measured on mpv 0.37,
    /// +20 to +400 ms after `play_aside`). Asking mpv would mix a second
    /// chime into the first, to be cut when the first ends (`amix
    /// duration=first`), or lose it while the first still loads. So when the
    /// gate is inside an aside (`AsideGate::sounding_aside`) the answer is
    /// `Idle` without a question: the next `play_aside` replaces the chime,
    /// and the gate swallows two asides in a row.
    ///
    /// **Armed but not yet inside** (the chime's `loadfile` written, its
    /// `start-file` not read yet) is not taken for idle: `armed` stays
    /// counted if that `start-file` never comes, and an `Idle` then, with a
    /// real file playing, would replace it. mpv is asked instead, and the
    /// gate looked at again once it has answered, since the chime's
    /// `start-file` may have been read meanwhile; otherwise the rare chime
    /// sent in those few milliseconds goes by mpv's own answer (most often
    /// `idle-active` true still: replaced as well).
    ///
    /// **A real load in flight is never replaced.** From a `play` or
    /// `load_list` until its `start-file`, mpv may still answer
    /// `idle-active` true (a load from idle or standby), and an `Idle` would
    /// send a `replace` cutting the file just asked for. The answer is
    /// `Silent`, without a question; and again after mpv's answer, should a
    /// load have been sent meanwhile. The chime is lost, never the load.
    ///
    /// **Paused** (`pause` true, audio parameters known), three more
    /// questions: `time-pos`, `duration`, `seekable`. A seekable file is put
    /// back at `time-pos` after the chime, and is `Silent` when it ends
    /// sooner than the chime plus `super::PAUSED_CHIME_END_MARGIN_S` (or has no
    /// position to go back to); a stream that is not seekable has no
    /// `resume_at` and no guard, its `duration` being the end of its cache
    /// (measured, mpv 0.37). Only an explicit `seekable: false` makes a
    /// stream: an unavailable or non-boolean answer is `Silent`.
    ///
    /// **A paused chime sounding is `Silent`, without a question**: mpv,
    /// unpaused for it, would answer `Live`, and a chime mixed over the
    /// music at full gain would bring the music back. The core, which knows
    /// it started that chime, handles a second one itself.
    async fn chime_route(&self, chime: std::time::Duration) -> Result<super::ChimeRoute> {
        use super::ChimeRoute;
        let settled = |route: ChimeRoute| {
            let gate = self.gate();
            if gate.loading() || gate.in_paused_chime() {
                ChimeRoute::Silent
            } else if gate.sounding_aside() {
                ChimeRoute::Idle
            } else {
                route
            }
        };
        if self.gate().loading() || self.gate().in_paused_chime() {
            return Ok(ChimeRoute::Silent);
        }
        if self.gate().sounding_aside() {
            return Ok(ChimeRoute::Idle);
        }
        let route = self.ask_chime_route(chime).await?;
        Ok(settled(route))
    }

    /// The named `loadfile` measured by the lock-sound spike on mpv 0.37
    /// (and present, by its command list, on 0.40): `replace`, and the
    /// file-local options `loop-file=no,pause=no`, so a global `pause` or a
    /// looping track setting neither silences nor repeats the chime; mpv
    /// reverts both once the chime ends (measured). The gate is armed
    /// **before** the command is written, since the chime's `start-file` can
    /// arrive before its response, and disarmed if the command fails.
    async fn play_aside(&self, uri: &str, af: Option<&str>) -> Result<()> {
        self.gate().arm();
        // A non-reference lock sound level adds its own `af` for this one
        // file (`lock_sound::idle_af`); mpv restores the chain at its end.
        let options = match af {
            Some(af) => format!("loop-file=no,pause=no,af={af}"),
            None => "loop-file=no,pause=no".to_string(),
        };
        let load = json!({"name": "loadfile", "url": uri, "flags": "replace", "options": options});
        if let Err(e) = self.ipc.command_named(load).await {
            self.gate().disarm();
            return Err(e);
        }
        Ok(())
    }

    /// The paused spike's sequence on mpv 0.37 (`kf48`, 20/20 clean): the
    /// chain, then the flush seek `absolute+keyframes`, then the unpause.
    ///
    /// **The flush.** Without it, the audio mpv had queued at the pause
    /// (`audio-buffer`, filtered by the chain of that moment) played at the
    /// unpause, at full gain, before the chime — every cycle. A seek to where
    /// the file stands drops it. **`+keyframes`, not `+exact`**: mpv trims
    /// an exact seek's lead-in samples after the filter chain, which cut up
    /// to a packet (512 frames of WAV there; 1152 for MP3, up to 4608 for
    /// FLAC) off the chime's attack in 8 of 20 cycles. A keyframe seek trims
    /// nothing, and the music it lands on early is at gain 0. The number is
    /// sent as `time-pos` gave it.
    ///
    /// The gate is set **before** the chain is written, so nothing of it can
    /// reach the core, and `progress` is read once, at the first chime.
    /// Within `PAUSED_CHIME_START_BUDGET`: a step past it is not sent and
    /// the call fails, the core then ends the chime at once. Nothing is
    /// touched before the pause's progress has been read in time: the gate
    /// is armed only then, so a start that gave up there leaves it as it
    /// was.
    async fn sound_while_paused(&self, chain: &str, resume_at: Option<f64>) -> Result<()> {
        let deadline = tokio::time::Instant::now() + PAUSED_CHIME_START_BUDGET;
        if !self.gate().in_paused_chime() {
            // What `progress` answers: the kept pause point after a restore.
            let frozen = self.gate().frozen();
            let progress = match frozen {
                Some(kept) => kept,
                None => {
                    let position = number_or_none(self.ask_until("time-pos", deadline).await);
                    let duration = number_or_none(self.ask_until("duration", deadline).await);
                    if tokio::time::Instant::now() >= deadline {
                        return Err(CommandTimeout.into());
                    }
                    Progress { position_s: position, duration_s: duration }
                }
            };
            self.gate().begin_paused_chime(progress);
        }
        self.act_until(&[json!("set_property"), json!("af"), json!(chain)], deadline).await?;
        if let Some(t) = resume_at {
            self.act_until(&[json!("seek"), json!(t), json!("absolute+keyframes")], deadline).await?;
        }
        self.act_until(&[json!("set_property"), json!("pause"), json!(false)], deadline).await?;
        Ok(())
    }

    /// The pause, the chain, its `audio-reconfig` awaited
    /// (`RESTORE_RECONFIG_WAIT`), then the restore seek `absolute+exact`:
    /// played back 0 or −1 sample from `resume_at` in every measured cycle
    /// (the −1 is the exact seek's float rounding, always so at 44.1 kHz).
    /// Never checked by reading `time-pos` afterwards: it once reported the
    /// restored value while the audio resumed 2.7 s off.
    ///
    /// Within `PAUSED_CHIME_END_BUDGET`, every step **written** even past
    /// it, only not awaited: mpv applies its commands in order, so one that
    /// answers late still ends paused, on the plain chain, put back. The
    /// restore seek counts as sent (its `playback-restart` to swallow) once
    /// written, unless mpv refused it.
    async fn end_sound_while_paused(&self, chain: &str, resume_at: Option<f64>) -> Result<()> {
        let deadline = tokio::time::Instant::now() + PAUSED_CHIME_END_BUDGET;
        let mut first_error = None;
        let mut keep = |r: Result<Value>| {
            let ok = r.is_ok();
            if let Err(e) = r {
                first_error.get_or_insert(e);
            }
            ok
        };
        keep(self.ipc.command_until(&[json!("set_property"), json!("pause"), json!(true)], deadline).await);
        let mut rebuilt = self.ipc.reconfigs.subscribe();
        let chain_written = keep(self.ipc.command_until(&[json!("set_property"), json!("af"), json!(chain)], deadline).await);
        let mut restore_sent = false;
        if let Some(t) = resume_at {
            let wait = RESTORE_RECONFIG_WAIT.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            if chain_written && tokio::time::timeout(wait, rebuilt.changed()).await.is_err() {
                tracing::debug!("lock sound while paused: no audio-reconfig after the chain, seeking anyway");
            }
            let seek = self.ipc.command_until(&[json!("seek"), json!(t), json!("absolute+exact")], deadline).await;
            // Unanswered is not refused: written, mpv may still apply it.
            restore_sent = match &seek {
                Ok(_) => true,
                Err(e) => e.is::<CommandTimeout>(),
            };
            keep(seek);
        }
        self.gate().end_paused_chime(restore_sent);
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::Event;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn command_receives_the_matching_response() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let ipc = MpvIpc::from_stream(client, tx);

        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_u64().unwrap();
            let resp = format!("{{\"error\":\"success\",\"data\":42,\"request_id\":{id}}}\n");
            w.write_all(resp.as_bytes()).await.unwrap();
        });

        let v = ipc.command(&[serde_json::json!("get_property"), serde_json::json!("volume")])
            .await
            .unwrap();
        assert_eq!(v, serde_json::json!(42));
    }

    /// The exact command measured to be honoured by the next `loadfile` on
    /// mpv 0.37, and never `set_property ytdl` (which reloads the hook
    /// script behind the load: see `set_ytdl`).
    #[tokio::test]
    async fn set_ytdl_sends_the_measured_commands() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };

        // Answers every command until the client goes away; returns them in order.
        let server = tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Some(line) = lines.next_line().await.unwrap() {
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let id = req["request_id"].as_u64().unwrap();
                let resp = format!("{{\"error\":\"success\",\"data\":null,\"request_id\":{id}}}\n");
                w.write_all(resp.as_bytes()).await.unwrap();
                seen.push(req["command"].clone());
            }
            seen
        });

        player.set_ytdl(Some(std::path::Path::new("/p/yt-dlp"))).await.unwrap();
        player.set_ytdl(None).await.unwrap();
        drop(player);
        assert_eq!(
            server.await.unwrap(),
            vec![
                serde_json::json!(["change-list", "script-opts", "append", "ytdl_hook-ytdl_path=/p/yt-dlp"]),
                serde_json::json!(["change-list", "script-opts", "append", "ytdl_hook-ytdl_path=/bin/false"]),
            ]
        );
    }

    /// The real thing, on a real mpv: another source's `Play` then a
    /// web-streaming `Play`, sent back to back in the core's order
    /// (`set_ytdl`, then the load), five times over. The regression: the old
    /// `set_property ytdl false/true` reloaded the hook script behind the
    /// load, and the web play reached the wrapper in 25 of 100 measured runs
    /// — five in a row would pass by chance about once in four hundred.
    ///
    /// Also held, on the same mpv: another source's load of a dead `http`
    /// stream and of a `ytdl://` address runs neither the wrapper of the web
    /// playback before it nor a `yt-dlp` found on the `PATH`.
    ///
    /// Needs `mpv` (the CI's rust job installs it); without it the test says
    /// so on stderr and returns, like the tests that need ffmpeg.
    #[tokio::test]
    async fn a_web_play_right_after_another_source_reaches_its_ytdlp_on_a_real_mpv() {
        use crate::player::Player;
        use std::os::unix::fs::PermissionsExt;
        if std::process::Command::new("mpv").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("mpv missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let calls = d.join("calls.log");
        let script = |path: &Path, body: String| {
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        std::fs::create_dir(d.join("pathbin")).unwrap();
        for (path, tag) in [(d.join("wrapper"), "WRAP"), (d.join("pathbin/yt-dlp"), "PATH")] {
            script(&path, format!("#!/bin/sh\necho {tag} >> '{}'\nexit 1\n", calls.display()));
        }
        // The production arguments, plus what a test machine needs: no
        // user configuration, no sound card, our stand-in first on the PATH.
        let mpv = d.join("mpv");
        script(
            &mpv,
            format!(
                "#!/bin/sh\nPATH='{}':\"$PATH\" exec mpv --no-config --ao=null \"$@\"\n",
                d.join("pathbin").display()
            ),
        );
        let (tx, mut rx) = mpsc::channel(1024);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let (player, _child) =
            start_test_mpv(&mpv, d, "", tx).await;
        let count = |tag: &str| std::fs::read_to_string(&calls).unwrap_or_default().lines().filter(|l| *l == tag).count();
        let wrapper = d.join("wrapper");
        for i in 0..5 {
            // Another source's Play: a dead radio.
            player.set_ytdl(None).await.unwrap();
            player.play(&format!("http://127.0.0.1:9/x{i}.mp3")).await.unwrap();
            // The web Play, right behind it.
            let before = count("WRAP");
            player.set_ytdl(Some(&wrapper)).await.unwrap();
            player.play(&format!("ytdl://https://example.invalid/v{i}")).await.unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while count("WRAP") == before {
                assert!(std::time::Instant::now() < deadline, "web play {i} never reached its yt-dlp");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            // Let the hook's second attempt land before counting again.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let (wrap, path) = (count("WRAP"), count("PATH"));
            // Another source's Play naming a ytdl:// address itself, then a
            // dead http stream given the time to fail.
            player.set_ytdl(None).await.unwrap();
            player.play(&format!("ytdl://http://127.0.0.1:9/y{i}")).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            player.set_ytdl(None).await.unwrap();
            player.play(&format!("http://127.0.0.1:9/z{i}.mp3")).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            assert_eq!((count("WRAP"), count("PATH")), (wrap, path), "run {i}: another source's load ran a yt-dlp");
        }
        assert_eq!(count("PATH"), 0, "a yt-dlp found on the PATH ran");
    }

    #[tokio::test]
    async fn property_change_becomes_an_event() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"media-title\",\"data\":\"FIP - Miles Davis\"}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":true}\n")
            .await
            .unwrap();
        // `file-loaded`, not `idle-active` turning false: see the doc on
        // that arm and on `Event::PlaybackActive` for why.
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"playlist-pos\",\"data\":3}\n")
            .await
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), Event::Title("FIP - Miles Davis".into()));
        // The `idle-active: true` sent above is the **first** observed
        // value: it describes the idle daemon's starting state, not a
        // stop, and is therefore swallowed (see
        // `the_first_observed_idle_is_not_a_stop`) — and produces no event
        // either way now, `idle-active` no longer being the source of
        // `PlaybackActive`.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(3));
    }

    #[tokio::test]
    async fn the_first_observed_idle_is_not_a_stop() {
        // mpv is launched as an idle daemon, and `observe_property`
        // immediately returns the current value: `idle-active = true`
        // therefore arrives before any playback. It is a starting state,
        // not a stop — but the core treats `PlaybackIdle` as the end of
        // what was playing (`playback = false`, and `Stop` notified to the
        // Source).
        //
        // Defect measured in practice: this event waits in the channel
        // while startup launches the first playback, and it is processed
        // right after. On **finite** content — a file — nothing catches
        // it: no "listening", rewind and forward greyed out, position
        // absent, until a play/pause reloads everything from the start. A
        // stream, on the other hand, would go back through the restart
        // branch (`expecting_stream`) and replay on its own, which masked
        // the defect on the radio side.
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        // In order: the observation's initial value (the idle daemon at
        // startup), a real load (`file-loaded`, not `idle-active` turning
        // false — see that arm's own doc), then a real stop at the end of
        // a playlist (`idle-active` back to `true`).
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":true}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":true}\n")
            .await
            .unwrap();

        // The first `true` is swallowed: nothing is emitted for it, and
        // the first event actually received is the real load.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        // The second `true`, though, follows a playback: it is a genuine
        // stop, and it must go through — otherwise the end of a playlist
        // would no longer display.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackIdle);
    }

    #[tokio::test]
    async fn a_reload_issued_while_already_playing_still_confirms_playback() {
        // Regression #5 (whole-branch review): `idle-active` turning false
        // is documented (unmeasured — no real mpv session available to
        // confirm it) to not toggle again on a reload issued while mpv is
        // already playing something else — already `false`, it stays
        // `false`, with no change event through it at all. `file-loaded`
        // fires on *every* load regardless: proven here by two of them
        // arriving with no `idle-active` message in between at all — the
        // old code, reading only `idle-active`, would have produced no
        // event whatsoever for either.
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
    }

    #[tokio::test]
    async fn mpv_error_bubbles_up_as_err() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let ipc = MpvIpc::from_stream(client, tx);
        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_u64().unwrap();
            let resp = format!("{{\"error\":\"invalid parameter\",\"request_id\":{id}}}\n");
            w.write_all(resp.as_bytes()).await.unwrap();
        });
        assert!(ipc.command(&[serde_json::json!("loadfile")]).await.is_err());
    }

    #[tokio::test]
    async fn icy_metadata_becomes_a_title_event() {
        // Real capture: shape of the `property-change` mpv emits for the
        // `metadata` property on an Icecast stream (SomaFM Groove Salad,
        // the only one of five streams measured to emit a usable
        // StreamTitle).
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(
            b"{\"event\":\"property-change\",\"name\":\"metadata\",\"data\":{\"icy-br\":\"128\",\"icy-title\":\"Mandrillus Sphynx - Bikwix\"}}\n",
        )
        .await
        .unwrap();
        assert_eq!(rx.recv().await.unwrap(), Event::IcyTitle("Mandrillus Sphynx - Bikwix".into()));
    }

    #[test]
    fn a_local_files_tags_give_the_four_fields() {
        // Payload recorded on the bench for an mp3 (ID3). FFmpeg normalizes
        // the keys: flac, ogg, opus, m4a and wav were checked and surface
        // under the same names, so a single grammar to know. `date` is one
        // of them and carries the long form here: without this assertion,
        // reading `year` from the wrong key (`year`, which FFmpeg does not
        // emit) would have gone unnoticed.
        let data = serde_json::json!({
            "title": "So What", "artist": "Miles Davis",
            "album": "Kind of Blue", "date": "1959-08-17", "encoder": "Lavf60.16.100"
        });
        let m = file_tags(&data).unwrap();
        assert_eq!(m.title.as_deref(), Some("So What"));
        assert_eq!(m.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(m.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(m.year, Some(1959));
        assert_eq!(m.origin.as_deref(), Some("tags"));
    }

    #[test]
    fn m4a_container_keys_are_ignored() {
        // Recorded on the bench: an m4a also surfaces container keys. Four
        // named keys are picked out, the object is never absorbed.
        let data = serde_json::json!({
            "title": "So What", "major_brand": "M4A ", "handler_name": "SoundHandler",
            "vendor_id": "[0][0][0][0]", "compatible_brands": "M4A mp42isom"
        });
        let m = file_tags(&data).unwrap();
        assert_eq!(m.title.as_deref(), Some("So What"));
        assert_eq!(m.artist, None);
        assert_eq!(m.album, None);
        assert_eq!(m.year, None);
    }

    #[test]
    fn an_icy_payload_produces_no_tag() {
        // The guard that protects the radio: some stations fill in a
        // `title` equal to the STATION NAME alongside an `icy-title` that
        // carries the real track. Preferring the former would be a silent
        // regression — the track's title replaced by the station's name.
        let data = serde_json::json!({
            "icy-br": "128", "icy-title": "Mandrillus Sphynx - Bikwix", "title": "OUI FM"
        });
        assert!(file_tags(&data).is_none());
        assert_eq!(icy_title(&data).as_deref(), Some("Mandrillus Sphynx - Bikwix"));
    }

    #[test]
    fn a_payload_with_nothing_readable_produces_no_tag() {
        // An empty enrichment would count as a response and would mask the
        // ICY: it must not exist.
        assert!(file_tags(&serde_json::json!({"encoder": "Lavf60.16.100"})).is_none());
        assert!(file_tags(&serde_json::json!({"title": "   "})).is_none());
        assert!(file_tags(&serde_json::json!({})).is_none());
        assert!(file_tags(&Value::Null).is_none());
    }

    /// The `metadata` property exactly as mpv 0.37 published it for each
    /// fixture of the 2026-11-03 measurement (see `file_lyrics`).
    #[test]
    fn file_lyrics_reads_the_keys_ffmpeg_exposes() {
        let lrc = "[00:01.00]First line\n[00:02.50]Second line";
        let measured = [
            // mp3, real USLT frame, language "eng".
            serde_json::json!({"encoder":"Lavf60.16.100","title":"T","artist":"A","lyrics-eng":lrc}),
            // mp3, real USLT frame with a content descriptor.
            serde_json::json!({"lyrics-desc-eng":lrc,"encoder":"Lavf60.16.100","title":"T","artist":"A"}),
            // mp3 tagged by ffmpeg (TXXX:USLT).
            serde_json::json!({"encoder":"Lavf60.16.100","artist":"A","lyrics":lrc,"title":"T"}),
            // flac, Vorbis comment LYRICS.
            serde_json::json!({"title":"T","artist":"A","LYRICS":lrc,"encoder":"Lavf60.16.100"}),
            // m4a, ©lyr atom, with the container keys an m4a also surfaces.
            serde_json::json!({"language":"und","artist":"A","minor_version":"512","title":"T",
                "compatible_brands":"M4A isomiso2","major_brand":"M4A ","encoder":"Lavf60.16.100",
                "vendor_id":"[0][0][0][0]","handler_name":"SoundHandler","lyrics":lrc}),
        ];
        for data in &measured {
            let l = file_lyrics(data).unwrap_or_else(|| panic!("no lyrics read from {data}"));
            assert_eq!(l.synced.as_deref(), Some(lrc), "{data}");
            assert_eq!(l.plain, None, "{data}");
        }
        // flac, Vorbis comment UNSYNCEDLYRICS: plain text.
        let unsynced = serde_json::json!({"title":"T","artist":"A","UNSYNCEDLYRICS":"plain words","encoder":"Lavf60.16.100"});
        assert_eq!(
            file_lyrics(&unsynced),
            Some(Lyrics { plain: Some("plain words".into()), synced: None })
        );
        // Words that are not LRC are plain text, not dropped.
        let untimed = serde_json::json!({"lyrics-eng":"Just words"});
        assert_eq!(file_lyrics(&untimed), Some(Lyrics { plain: Some("Just words".into()), synced: None }));
        // Nothing to read: no lyrics, and not an empty set.
        assert_eq!(file_lyrics(&serde_json::json!({"title":"T","lyrics-eng":"   "})), None);
        assert_eq!(file_lyrics(&serde_json::json!({"title":"T"})), None);
    }

    #[test]
    fn file_lyrics_stays_silent_on_a_stream() {
        // What a station puts beside its ICY title is not the song's words.
        let stream = serde_json::json!({"icy-title":"A - T","icy-name":"Station","lyrics":"[00:01.00]x"});
        assert_eq!(file_lyrics(&stream), None);
        assert_eq!(file_tags_event(&stream), None);
    }

    #[test]
    fn file_tags_are_emitted_for_text_or_lyrics_alone() {
        let lrc = "[00:01.00]Hi";
        let both = file_tags_event(&serde_json::json!({"title":"T","LYRICS":lrc})).expect("emitted");
        let Event::FileTags { track, lyrics } = both else { panic!("{both:?}") };
        assert_eq!(track.and_then(|t| t.title).as_deref(), Some("T"));
        assert!(lyrics.is_some());
        // A file whose only tag is its words still reaches the core.
        let alone = file_tags_event(&serde_json::json!({"LYRICS":lrc})).expect("emitted");
        assert!(matches!(alone, Event::FileTags { track: None, lyrics: Some(_) }), "{alone:?}");
        // Text without lyrics: emitted with `None`, which clears the words a
        // previous song of the same stream carried.
        let text = file_tags_event(&serde_json::json!({"title":"T2"})).expect("emitted");
        assert!(matches!(text, Event::FileTags { track: Some(_), lyrics: None }), "{text:?}");
        assert_eq!(file_tags_event(&serde_json::json!({"encoder":"Lavf"})), None);
    }

    #[test]
    fn icy_title_ignores_empty_and_absent() {
        // Measured cases: Radio Nova sends an empty StreamTitle, FIP sends
        // no ICY header at all (no icy-metaint whatsoever).
        assert_eq!(icy_title(&serde_json::json!({"icy-title": ""})), None);
        assert_eq!(icy_title(&serde_json::json!({"icy-title": "   "})), None);
        assert_eq!(icy_title(&serde_json::json!({"icy-br": "128"})), None);
        assert_eq!(icy_title(&serde_json::json!({})), None);
        // `metadata` is null as long as no file is loaded.
        assert_eq!(icy_title(&Value::Null), None);
        assert_eq!(icy_title(&serde_json::json!("not an object")), None);
        // A non-textual value must not panic.
        assert_eq!(icy_title(&serde_json::json!({"icy-title": 42})), None);
    }

    #[test]
    fn icy_title_tolerates_case_and_trims() {
        assert_eq!(
            icy_title(&serde_json::json!({"Icy-Title": "  Miles Davis - So What "})).as_deref(),
            Some("Miles Davis - So What")
        );
        assert_eq!(icy_title(&serde_json::json!({"ICY-TITLE": "x"})).as_deref(), Some("x"));
    }

    #[test]
    fn the_path_property_is_observed() {
        // Without it, the stall journal could not name what stalled.
        assert!(OBSERVED.contains(&"path"), "without it, a stall names nothing");
    }

    #[test]
    fn a_stream_is_never_read_as_a_file() {
        // Only attempted on a path with no scheme.
        assert!(read_media_file("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        assert!(read_media_file("http://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(read_media_file("/does/not/exist.flac").is_none());
    }

    /// The embedded cover of a local file, as `read_media_file` finds it.
    fn embedded_cover(path: &str) -> Option<crate::cover::CoverSource> {
        read_media_file(path).and_then(|m| m.cover)
    }

    /// Builds a real mp3 with an embedded cover, via ffmpeg, or returns
    /// `None` if it is absent.
    ///
    /// As in `ritornello-plugin-files::duration`: no binary checked into
    /// the repo, and the test is skipped rather than failed where ffmpeg is
    /// missing — it is a development tool, not a core dependency.
    ///
    /// `source_image` is an `lavfi` filter, hence the embedded image.
    /// `read_media_file` writes anything to a shared location, so
    /// two parallel tests embedding the same image can no longer collide the
    /// way an earlier version of this fixture had to guard against; callers
    /// nonetheless still pass their own filter, which is convenient to keep
    /// assertions about `content` unambiguous per test.
    ///
    /// `pub(crate)` so `cover::tests` can build a fixture that carries a
    /// real embedded picture, rather than a second copy of this function
    /// drifting apart from this one.
    pub(crate) fn mp3_with_cover_from(dir: &Path, source_image: &str) -> Option<std::path::PathBuf> {
        let image = dir.join("cover.jpg");
        let output = dir.join("with_cover.mp3");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", source_image])
            .args(["-frames:v", "1"])
            .arg(&image)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            && std::process::Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg("sine=frequency=440:duration=1")
                .arg("-i")
                .arg(&image)
                .args(["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "copy"])
                .args(["-id3v2_version", "3"])
                .args(["-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"])
                .arg(&output)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        ok.then_some(output)
    }

    /// This module's image.
    fn mp3_with_cover(dir: &Path) -> Option<std::path::PathBuf> {
        mp3_with_cover_from(dir, "color=c=red:s=16x16:d=1")
    }

    /// A real mp3 carrying the three text tags and a date, via ffmpeg, or
    /// `None` where ffmpeg is missing. `pub(crate)` so the core's armed-track
    /// tests read the very file this module's test reads.
    pub(crate) fn tagged_mp3(dir: &Path) -> Option<std::path::PathBuf> {
        let output = dir.join("tagged.mp3");
        std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:a", "libmp3lame", "-id3v2_version", "3"])
            .args(["-metadata", "title=Alpha", "-metadata", "artist=Beta", "-metadata", "album=Gamma"])
            .args(["-metadata", "date=1971"])
            .arg(&output)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            .then_some(output)
    }

    #[test]
    fn a_declared_file_yields_the_tags_mpv_would_have_read() {
        let dir = tempfile::tempdir().unwrap();
        let Some(f) = tagged_mp3(dir.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let MediaFile { tags, lyrics, cover } = read_media_file(f.to_str().unwrap()).expect("a readable file must answer");
        let tags = tags.expect("the file carries tags");
        assert_eq!(tags.title.as_deref(), Some("Alpha"));
        assert_eq!(tags.artist.as_deref(), Some("Beta"));
        assert_eq!(tags.album.as_deref(), Some("Gamma"));
        assert_eq!(tags.year, Some(1971));
        assert!(tags.duration_s.is_some(), "the container's duration is known without playing it");
        assert_eq!(tags.origin.as_deref(), Some(crate::metadata::ORIGIN_TAGS));
        assert_eq!(cover, None, "no picture in this file");
        assert_eq!(lyrics, None, "no words in this file");
    }

    #[test]
    fn a_file_read_refuses_what_lofty_cannot_open() {
        assert!(read_media_file("http://x/y.mp3").is_none(), "a URL is not a file to read");
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.mp3");
        std::fs::write(&text, b"not audio at all, only words").unwrap();
        assert!(read_media_file(text.to_str().unwrap()).is_none(), "a file that is not audio has nothing to say");
        assert!(read_media_file("/does/not/exist.flac").is_none());
        assert!(read_media_file(dir.path().to_str().unwrap()).is_none(), "a directory is not a track");
    }

    /// A FIFO named as the armed file: opening it would block until a writer
    /// came, under `Health::bounded`, which would then mark the whole mount
    /// unreachable. The read must refuse it at once.
    ///
    /// **[MUTATION]** the `is_file()` guard removed from `read_media_file` →
    /// the `recv_timeout` assertion fires (the open blocks).
    #[cfg(unix)]
    #[test]
    fn a_file_read_refuses_a_fifo_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("track.flac");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !matches!(made, Ok(s) if s.success()) {
            eprintln!("mkfifo unavailable: skipping test");
            return;
        }
        let path = fifo.to_str().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_media_file(&path).is_none());
        });
        let refused = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("reading a FIFO must not block");
        assert!(refused, "a FIFO is not a track");
    }

    #[test]
    fn an_embedded_cover_is_probed_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let Some(f) = mp3_with_cover(dir.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let before = temp_cover_files();
        let r = embedded_cover(f.to_str().unwrap()).expect("an embedded cover was expected");
        let crate::cover::CoverSource::Embedded { audio, content } = r else {
            panic!("an embedded cover must yield CoverSource::Embedded");
        };
        assert_eq!(audio, std::path::Path::new(f.to_str().unwrap()));
        assert!(!content.is_empty());
        // The point of the whole rework: nothing is written to the temp dir.
        assert_eq!(temp_cover_files(), before, "the probe must write no file");
    }

    /// Files left in the system temp dir by a previous design. Kept as a
    /// guard: this set must never grow.
    fn temp_cover_files() -> Vec<std::path::PathBuf> {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return Vec::new() };
        let mut v: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ritornello-cover-"))
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn two_tracks_of_the_same_album_share_one_cache_key() {
        let dir = tempfile::tempdir().unwrap();
        // An image of its own for this test: see `mp3_with_cover_from`.
        let Some(track1) = mp3_with_cover_from(dir.path(), "color=c=blue:s=24x24:d=1") else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        // Two distinct track files carrying the same cover: the common
        // case of an album, and the one path-based naming used to charge
        // fifteen times over for a single image.
        let track2 = dir.path().join("track_2.mp3");
        std::fs::copy(&track1, &track2).unwrap();

        let r1 = embedded_cover(track1.to_str().unwrap()).expect("a cover was expected");
        let r2 = embedded_cover(track2.to_str().unwrap()).expect("a cover was expected");
        let crate::cover::CoverSource::Embedded { content: c1, .. } = &r1 else {
            panic!("an embedded cover must yield CoverSource::Embedded");
        };
        let crate::cover::CoverSource::Embedded { content: c2, .. } = &r2 else {
            panic!("an embedded cover must yield CoverSource::Embedded");
        };
        assert_eq!(c1, c2, "two tracks with an identical embedded cover must yield the same content");
        // The two `audio` paths differ (track1, track2): the dedup that
        // matters is `cover::key`'s, which hashes the content, never the
        // audio path — see its doc.
        assert_eq!(
            crate::cover::key(&r1),
            crate::cover::key(&r2),
            "two tracks with an identical cover must share the same cache key"
        );
    }

    #[test]
    fn all_useful_properties_are_observed() {
        // Without `observe_property`, mpv never pushes the property: the
        // ICY layer would stay silent without any `icy_title` test
        // noticing. Since `start` launches a real mpv process, it is the
        // list it iterates over that is checked here.
        assert!(OBSERVED.contains(&"metadata"), "without it, no ICY title ever arrives");
        assert!(OBSERVED.contains(&"idle-active"), "without it, no more restart after a drop");
        assert!(OBSERVED.contains(&"media-title"));
        assert!(OBSERVED.contains(&"playlist-pos"));
        assert!(OBSERVED.contains(&"paused-for-cache"), "without it, a cut leaves no trace");
    }

    /// The sequence mpv 0.37 produced against a server that went silent for
    /// 8 s without closing, the payloads as captured: the unavailable
    /// property at load (no `data`), `false` once playing, `true` into the
    /// silence, `false` when data is back.
    #[test]
    fn a_silence_gives_one_line_when_it_begins_and_its_duration_when_it_ends() {
        let t0 = std::time::Instant::now();
        let at = |s: f64| t0 + std::time::Duration::from_secs_f64(s);
        let mut stall = Stall::default();
        assert_eq!(stall.change(&Value::Null, at(0.10)), None);
        assert_eq!(stall.change(&json!(false), at(0.71)), None);
        assert_eq!(stall.change(&json!(true), at(5.37)), Some(StallNote::Began));
        // A repeated `true` is the same silence, not a second one: its
        // start must not move.
        assert_eq!(stall.change(&json!(true), at(9.0)), None);
        assert_eq!(
            stall.change(&json!(false), at(14.37)),
            Some(StallNote::Ended(std::time::Duration::from_secs(9)))
        );
        assert_eq!(stall.change(&json!(false), at(15.0)), None);
    }

    /// Two ways a silence ends without data coming back, both measured or
    /// documented: the server closes (`false`, then `end-file`), or the
    /// listener stops or changes station while mpv waits (the property
    /// becomes unavailable, a change with no `data`). Either way the stall
    /// is over and its duration is known; leaving it open would date the
    /// next one from the wrong instant.
    #[test]
    fn a_silence_ended_by_a_close_or_a_stop_still_gives_its_duration() {
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        let mut stall = Stall::default();
        assert_eq!(stall.change(&json!(true), at(5)), Some(StallNote::Began));
        assert_eq!(
            stall.change(&Value::Null, at(11)),
            Some(StallNote::Ended(std::time::Duration::from_secs(6)))
        );
        assert_eq!(stall.change(&json!(true), at(20)), Some(StallNote::Began));
        assert_eq!(
            stall.change(&json!(false), at(23)),
            Some(StallNote::Ended(std::time::Duration::from_secs(3)))
        );
    }

    #[test]
    fn an_absent_variable_gives_the_default_silently() {
        assert_eq!(audio_buffer_setting(None), AUDIO_BUFFER_DEFAULT);
        assert_eq!(readahead_setting(None), READAHEAD_DEFAULT);
    }

    #[test]
    fn a_valid_value_is_kept() {
        assert_eq!(audio_buffer_setting(Some("1.5")), 1.5);
        assert_eq!(audio_buffer_setting(Some("  2  ")), 2.0);
        assert_eq!(readahead_setting(Some("30")), 30.0);
        // 0 is legitimate: it is how to fall back to the most responsive
        // behavior, at the cost of robustness.
        assert_eq!(audio_buffer_setting(Some("0")), 0.0);
    }

    #[test]
    fn an_invalid_value_falls_back_to_the_default() {
        for raw in ["", "abc", "-1", "1,5", "NaN", "inf"] {
            assert_eq!(audio_buffer_setting(Some(raw)), AUDIO_BUFFER_DEFAULT, "raw={raw:?}");
        }
        // Beyond the upper bound: mpv would refuse above 10 s for the
        // output buffer, and an oversized read-ahead costs memory with no
        // benefit.
        assert_eq!(audio_buffer_setting(Some("42")), AUDIO_BUFFER_DEFAULT);
        assert_eq!(readahead_setting(Some("999")), READAHEAD_DEFAULT);
    }

    #[test]
    fn the_defaults_reproduce_mpvs_own() {
        // mpv 0.37: --audio-buffer=0.2 and --demuxer-readahead-secs=1
        // (measured via `mpv --list-options`). This module makes both
        // configurable without changing the default behavior: with no
        // variable set, mpv must behave exactly as if launched without
        // these options. Any drift in these values is an audio behavior
        // change that must be intentional, not a side effect — hence this
        // test.
        assert_eq!(audio_buffer_setting(None), 0.2);
        assert_eq!(readahead_setting(None), 1.0);
    }

    #[test]
    fn the_arguments_carry_both_buffers() {
        let args = mpv_args(std::path::Path::new("/run/rp/mpv.sock"), "/dev/sr0", 0.5, 10.0, "");
        assert!(args.contains(&"--audio-buffer=0.5".to_string()), "{args:?}");
        assert!(args.contains(&"--demuxer-readahead-secs=10".to_string()), "{args:?}");
        // The pre-existing arguments must not have been lost along the way.
        assert!(args.contains(&"--idle=yes".to_string()));
        assert!(args.contains(&"--no-video".to_string()));
        // Without it, every track of a folder holding a `cover.jpg` started
        // 2.7 s late on the device: mpv loaded the image itself.
        assert!(args.contains(&"--cover-art-auto=no".to_string()));
        assert!(args.contains(&"--no-terminal".to_string()));
        assert!(args.contains(&"--input-ipc-server=/run/rp/mpv.sock".to_string()));
        assert!(args.contains(&"--cdda-device=/dev/sr0".to_string()));
        // The yt-dlp hook: no http(s) fallback ever, a program that does nothing.
        assert!(args.contains(&"--script-opts=ytdl_hook-exclude=.,ytdl_hook-ytdl_path=/bin/false".to_string()), "{args:?}");
    }

    #[test]
    fn mpv_args_carry_af_only_when_set() {
        let socket = std::path::Path::new("/run/rp/mpv.sock");
        let empty = mpv_args(socket, "/dev/sr0", 0.5, 10.0, "");
        assert!(!empty.iter().any(|a| a.starts_with("--af")), "{empty:?}");
        let chain = "@eqpre:lavfi=[highshelf=f=10:t=q:w=0.707:g=0],@eq0:lavfi=[equalizer=f=31:t=o:w=1:g=0]";
        let set = mpv_args(socket, "/dev/sr0", 0.5, 10.0, chain);
        let afs: Vec<_> = set.iter().filter(|a| a.starts_with("--af")).collect();
        assert_eq!(afs, vec![&format!("--af={chain}")], "{set:?}");
    }

    #[tokio::test]
    async fn start_file_becomes_file_starting() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(b"{\"event\":\"start-file\",\"playlist_entry_id\":1}\n").await.unwrap();
        assert!(matches!(rx.recv().await, Some(Event::FileStarting)));
    }

    #[tokio::test]
    async fn playback_restart_becomes_playback_restarted() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(b"{\"event\":\"playback-restart\"}\n").await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(matches!(ev, Ok(Some(Event::PlaybackRestarted))), "{ev:?}");
    }

    /// The real thing: `af-command` with its fifth element reaches a running
    /// mpv holding the equalizer chain (measured on mpv 0.37: without the
    /// target it fails), and `set_audio_filters` replaces the chain.
    ///
    /// Needs `mpv`; without it the test says so on stderr and returns, like
    /// the tests that need ffmpeg.
    #[tokio::test]
    async fn af_command_reaches_a_running_mpv() {
        use crate::player::Player;
        if std::process::Command::new("mpv").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("mpv missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mpv = d.join("mpv");
        std::fs::write(&mpv, "#!/bin/sh\nexec mpv --no-config --ao=null \"$@\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mpv, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Built through the model, so this cannot drift from the real format.
        let chain = crate::equalizer::af_chain(&crate::equalizer::EqualizerSettings {
            enabled: true,
            ..Default::default()
        });
        let (tx, mut rx) = mpsc::channel(1024);
        let (player, _child) =
            start_test_mpv(&mpv, d, &chain, tx).await;
        let graph = |v: &Value, label: &str| -> Option<String> {
            v.as_array()?.iter().find(|f| f["label"] == json!(label))?["params"]["graph"].as_str().map(str::to_string)
        };
        let af = player.ipc.command(&[json!("get_property"), json!("af")]).await.unwrap();
        assert_eq!(graph(&af, "eq5").as_deref(), Some("equalizer=f=1000:t=o:w=1:g=0"), "{af}");
        assert_eq!(graph(&af, "eqpre").as_deref(), Some("highshelf=f=10:t=q:w=0.707:g=0"), "{af}");

        // Idle, the two facts `core/equalizer.rs` relies on: `af-command`
        // is refused (no live chain), setting `af` is accepted.
        assert!(
            player.audio_filter_command(&crate::equalizer::band_command(5, 6)).await.is_err(),
            "an idle mpv must refuse af-command"
        );
        let mut idle_eq = crate::equalizer::EqualizerSettings { enabled: true, ..Default::default() };
        idle_eq.gains[5] = 3;
        player.set_audio_filters(&crate::equalizer::af_chain(&idle_eq)).await.expect("setting af on an idle mpv");
        let af = player.ipc.command(&[json!("get_property"), json!("af")]).await.unwrap();
        assert_eq!(graph(&af, "eq5").as_deref(), Some("equalizer=f=1000:t=o:w=1:g=3"), "{af}");
        player.set_audio_filters(&chain).await.unwrap();

        // The chain only exists while something plays (an idle mpv answers
        // "error running command" to any `af-command`): a generated tone.
        // From `playback-restart` on — what the core waits for before
        // commanding the chain — the first `af-command` must succeed.
        player.play("av://lavfi:sine=frequency=440:duration=30").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(ev) = rx.recv().await {
                if ev == Event::PlaybackRestarted {
                    return;
                }
            }
            panic!("event channel closed before playback-restart");
        })
        .await
        .expect("no playback-restart within 10 s");
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let cmd = crate::equalizer::band_command(5, 6);
        player.audio_filter_command(&cmd).await.expect("af-command right after playback-restart");
        // The same command without its target is the failure the fifth
        // element guards against, on a chain that does exist.
        assert!(
            player.ipc.command(&[json!("af-command"), json!("eq5"), json!("g"), json!("6")]).await.is_err(),
            "af-command without a target must fail on mpv"
        );
        let preamp = crate::equalizer::preamp_command(-6.0);
        player.audio_filter_command(&preamp).await.expect("preamp af-command");

        player.set_audio_filters("").await.unwrap();
        let af = player.ipc.command(&[json!("get_property"), json!("af")]).await.unwrap();
        assert_eq!(af.as_array().map(Vec::len), Some(0), "{af}");
    }

    /// The chime files read by a running mpv: the mix of a file's `amovie`
    /// into a playing mono 48 kHz stream (the channel-mapping branch of
    /// `lock_sound::mix_filter`), then the idle load of the same file. mpv
    /// refuses a malformed filter at `set_property af`, so the mix being
    /// accepted is the check; the load is accepted when mpv opens the file.
    ///
    /// Needs `mpv`; without it the test says so on stderr and returns.
    #[tokio::test]
    async fn the_chime_files_are_read_by_a_running_mpv() {
        use crate::lock_sound::{LockSound, Motif};
        use crate::player::Player;
        if mpv_missing() {
            eprintln!("mpv missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mpv = d.join("mpv");
        wrapper(&mpv, "#!/bin/sh\nexec mpv --no-config --ao=null \"$@\"\n");
        let (tx, mut rx) = mpsc::channel(1024);
        let (player, _child) = start_test_mpv(&mpv, d, "", tx).await;
        assert!(player.chime_dir().join("a-lock.wav").exists(), "start writes the chime files");

        player.play("av://lavfi:sine=frequency=440:sample_rate=48000:duration=30").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(ev) = rx.recv().await {
                if ev == Event::PlaybackRestarted {
                    return;
                }
            }
            panic!("event channel closed before playback-restart");
        })
        .await
        .expect("no playback-restart within 10 s");
        tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let mix = crate::lock_sound::mix_filter(player.chime_dir(), LockSound::A, Motif::Lock, 48000, 1, crate::lock_sound::LockVolume::Two).unwrap();
        player.set_audio_filters(&mix).await.expect("mpv accepts the mix of the chime file");
        let af = player.ipc.command(&[json!("get_property"), json!("af")]).await.unwrap();
        assert_eq!(af.as_array().map(Vec::len), Some(1), "{af}");
        player.set_audio_filters("").await.unwrap();

        let uri = crate::lock_sound::idle_uri(player.chime_dir(), LockSound::A, Motif::Lock).unwrap();
        player.play_aside(&uri, None).await.expect("mpv loads the chime file");
    }

    /// mpv answers `null` on `time-pos` when nothing is loaded, and an
    /// **error** when the property is unavailable. Both say the same
    /// thing — "I don't know" — and neither is a failure to surface: an
    /// unknown position is a normal case, not an incident.
    #[test]
    fn an_absent_or_null_value_becomes_none() {
        assert_eq!(number_or_none(Ok(serde_json::json!(87.4))), Some(87.4));
        assert_eq!(number_or_none(Ok(serde_json::Value::Null)), None);
        assert_eq!(number_or_none(Err(anyhow::anyhow!("property unavailable"))), None);
    }

    /// A negative position does not exist, and mpv briefly produces one at
    /// a file's startup (measured: `-0.02`). Publishing it would display a
    /// bar going backwards.
    #[test]
    fn a_negative_value_becomes_none() {
        assert_eq!(number_or_none(Ok(serde_json::json!(-0.02))), None);
    }

    /// An idle chime alone, from its `loadfile` response to mpv back at
    /// `idle-active: true`. Captured on mpv 0.37.0 by the lock-sound spike,
    /// 2026-11-16 (fixture 1, run 1; all 20 runs had this shape).
    const IDLE_CHIME: &str = r#"{"data":{"playlist_entry_id":1},"request_id":11,"error":"success"}
{"event":"start-file","playlist_entry_id":1}
{"event":"property-change","id":1,"name":"media-title","data":"lavfi:aevalsrc=exprs='if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))':s=48000:d=0.74"}
{"event":"property-change","id":3,"name":"idle-active","data":false}
{"event":"property-change","id":4,"name":"playlist-pos","data":0}
{"event":"property-change","id":6,"name":"path","data":"av://lavfi:aevalsrc=exprs='if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))':s=48000:d=0.74"}
{"event":"audio-reconfig"}
{"event":"audio-reconfig"}
{"event":"file-loaded"}
{"event":"property-change","id":2,"name":"metadata","data":{}}
{"event":"property-change","id":7,"name":"paused-for-cache","data":false}
{"event":"audio-reconfig"}
{"event":"playback-restart"}
{"event":"audio-reconfig"}
{"event":"end-file","reason":"eof","playlist_entry_id":1}
{"event":"audio-reconfig"}
{"event":"idle"}
{"event":"property-change","id":1,"name":"media-title"}
{"event":"property-change","id":2,"name":"metadata"}
{"event":"property-change","id":3,"name":"idle-active","data":true}"#;

    /// What follows `IDLE_CHIME` at once, which the spike's capture stopped
    /// short of: mpv setting back what the chime moved. Captured on mpv
    /// 0.37.0 for this test module, 2026-11-16, same observe ids, with and
    /// without the spike's `loop-file inf` and `playlist-start 2`.
    const IDLE_CHIME_TAIL: &str = r#"{"event":"property-change","id":4,"name":"playlist-pos","data":-1}
{"event":"property-change","id":6,"name":"path"}
{"event":"property-change","id":7,"name":"paused-for-cache"}"#;

    /// The observations' initial values on a fresh idle mpv, right after the
    /// seven `observe_property` responses (same capture).
    const STARTUP: &str = r#"{"event":"property-change","id":1,"name":"media-title"}
{"event":"property-change","id":2,"name":"metadata"}
{"event":"property-change","id":3,"name":"idle-active","data":true}
{"event":"property-change","id":4,"name":"playlist-pos","data":-1}
{"event":"property-change","id":5,"name":"chapter"}
{"event":"property-change","id":6,"name":"path"}
{"event":"property-change","id":7,"name":"paused-for-cache"}"#;

    /// An idle chime interrupted by a real `loadfile` sent right after the
    /// chime's response, until the real file's `playback-restart`. Captured
    /// on mpv 0.37.0 by the lock-sound spike, 2026-11-16 (fixture 2, run 1:
    /// the single coalesced `playlist-pos: 0` falls inside the chime).
    const INTERRUPTED_RUN_1: &str = r#"{"data":{"playlist_entry_id":21},"request_id":91,"error":"success"}
{"data":{"playlist_entry_id":22},"request_id":92,"error":"success"}
{"event":"start-file","playlist_entry_id":21}
{"event":"property-change","id":1,"name":"media-title","data":"lavfi:aevalsrc=exprs='if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))':s=48000:d=0.74"}
{"event":"property-change","id":3,"name":"idle-active","data":false}
{"event":"property-change","id":4,"name":"playlist-pos","data":0}
{"event":"property-change","id":6,"name":"path","data":"av://lavfi:aevalsrc=exprs='if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))':s=48000:d=0.74"}
{"event":"end-file","reason":"stop","playlist_entry_id":21}
{"event":"start-file","playlist_entry_id":22}
{"event":"property-change","id":1,"name":"media-title","data":"lavfi:sine=frequency=440:d=2"}
{"event":"property-change","id":6,"name":"path","data":"av://lavfi:sine=frequency=440:d=2"}
{"event":"audio-reconfig"}
{"event":"audio-reconfig"}
{"event":"file-loaded"}
{"event":"property-change","id":2,"name":"metadata","data":{}}
{"event":"property-change","id":7,"name":"paused-for-cache","data":false}
{"event":"audio-reconfig"}
{"event":"playback-restart"}"#;

    /// Fixture 2, run 2: the most frequent shape (16 of 20 runs), where the
    /// second response falls after the chime's `start-file` and
    /// `playlist-pos: 0` after the real file's `media-title`.
    const INTERRUPTED_RUN_2: &str = r#"{"data":{"playlist_entry_id":23},"request_id":95,"error":"success"}
{"event":"start-file","playlist_entry_id":23}
{"data":{"playlist_entry_id":24},"request_id":96,"error":"success"}
{"event":"property-change","id":1,"name":"media-title","data":"lavfi:aevalsrc=exprs='if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))':s=48000:d=0.74"}
{"event":"property-change","id":3,"name":"idle-active","data":false}
{"event":"property-change","id":6,"name":"path","data":"av://lavfi:aevalsrc=exprs='if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))':s=48000:d=0.74"}
{"event":"end-file","reason":"stop","playlist_entry_id":23}
{"event":"start-file","playlist_entry_id":24}
{"event":"property-change","id":1,"name":"media-title","data":"lavfi:sine=frequency=440:d=2"}
{"event":"property-change","id":4,"name":"playlist-pos","data":0}
{"event":"property-change","id":6,"name":"path","data":"av://lavfi:sine=frequency=440:d=2"}
{"event":"audio-reconfig"}
{"event":"audio-reconfig"}
{"event":"file-loaded"}
{"event":"property-change","id":2,"name":"metadata","data":{}}
{"event":"property-change","id":7,"name":"paused-for-cache","data":false}
{"event":"audio-reconfig"}
{"event":"playback-restart"}"#;

    fn lines_of(fixture: &str) -> Vec<Value> {
        fixture.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    /// What the pump hands on for these lines: responses never reach the
    /// gate; an admitted line is followed by what the gate replays after it.
    fn feed(gate: &mut AsideGate, lines: &[Value]) -> Vec<Value> {
        let mut admitted = Vec::new();
        for line in lines.iter().filter(|l| l.get("request_id").is_none()) {
            if gate.admit(line) {
                admitted.push(line.clone());
                admitted.extend(gate.take_replay());
            }
        }
        admitted
    }

    /// The chime, then mpv setting back what it moved: not one line passes,
    /// and the next load passes with nothing replayed in front of it.
    #[test]
    fn aside_gate_swallows_the_captured_idle_chime() {
        let mut gate = AsideGate::default();
        let startup = lines_of(STARTUP);
        assert_eq!(feed(&mut gate, &startup), startup);
        gate.arm();
        let mut chime = lines_of(IDLE_CHIME);
        chime.extend(lines_of(IDLE_CHIME_TAIL));
        assert_eq!(feed(&mut gate, &chime), Vec::<Value>::new());
        let real = json!({"event":"start-file","playlist_entry_id":2});
        assert_eq!(feed(&mut gate, std::slice::from_ref(&real)), vec![real], "the load after the chime must reach the core");
        // And a stop after it is a stop again.
        let stop = json!({"event":"property-change","id":3,"name":"idle-active","data":true});
        assert!(gate.admit(&json!({"event":"property-change","id":3,"name":"idle-active","data":false})));
        assert!(gate.admit(&stop));
    }

    /// Wherever the second response and `playlist-pos` fall (run 1, then
    /// run 2's shape), the real load passes through from its `start-file`
    /// on — and the `playlist-pos: 0` swallowed inside the chime in run 1,
    /// which mpv will not notify again, is replayed right behind it.
    #[test]
    fn a_real_load_during_an_aside_passes_through() {
        let run1 = lines_of(INTERRUPTED_RUN_1);
        let real_start = run1.iter().position(|l| *l == json!({"event":"start-file","playlist_entry_id":22})).unwrap();
        let mut gate = AsideGate::default();
        feed(&mut gate, &lines_of(STARTUP));
        gate.arm();
        let mut expected = vec![run1[real_start].clone()];
        expected.push(json!({"event":"property-change","id":4,"name":"playlist-pos","data":0}));
        expected.extend(run1[real_start + 1..].iter().cloned());
        assert_eq!(feed(&mut gate, &run1), expected);
        // The chime's `idle-active: false` was swallowed and mpv will not
        // repeat it for the real file: the real stop must still pass.
        let stop = json!({"event":"property-change","id":3,"name":"idle-active","data":true});
        assert_eq!(feed(&mut gate, std::slice::from_ref(&stop)), vec![stop]);

        let run2 = lines_of(INTERRUPTED_RUN_2);
        let real_start = run2.iter().position(|l| *l == json!({"event":"start-file","playlist_entry_id":24})).unwrap();
        let mut gate = AsideGate::default();
        feed(&mut gate, &lines_of(STARTUP));
        gate.arm();
        // Nothing swallowed carried a track number: nothing to replay.
        assert_eq!(feed(&mut gate, &run2), run2[real_start..].to_vec());
    }

    /// Should mpv leave the chime's `playlist-pos: 0` in place after idle —
    /// the spike's capture, cut at `idle-active: true`, read that way — a
    /// later real load at position 0 would never be notified: the swallowed
    /// value is replayed behind that load's `start-file`, and a real
    /// notification after it still wins.
    #[test]
    fn a_track_number_swallowed_by_an_idle_chime_is_replayed_at_the_next_real_load() {
        let mut gate = AsideGate::default();
        // What the core last saw: the previous content stopped at -1.
        assert!(gate.admit(&json!({"event":"property-change","id":4,"name":"playlist-pos","data":-1})));
        gate.arm();
        assert_eq!(feed(&mut gate, &lines_of(IDLE_CHIME)), Vec::<Value>::new());
        let real = json!({"event":"start-file","playlist_entry_id":2});
        let real_pos = json!({"event":"property-change","id":4,"name":"playlist-pos","data":2});
        assert_eq!(
            feed(&mut gate, &[real.clone(), real_pos.clone()]),
            vec![real.clone(), json!({"event":"property-change","id":4,"name":"playlist-pos","data":0}), real_pos],
        );
        // Replayed once only.
        assert_eq!(feed(&mut gate, std::slice::from_ref(&real)), vec![real.clone()]);

        // A swallowed value equal to what the core already holds is what
        // mpv would not have re-sent without the chime: nothing replayed.
        let mut gate = AsideGate::default();
        assert!(gate.admit(&json!({"event":"property-change","id":4,"name":"playlist-pos","data":0})));
        gate.arm();
        assert_eq!(feed(&mut gate, &lines_of(IDLE_CHIME)), Vec::<Value>::new());
        assert_eq!(feed(&mut gate, std::slice::from_ref(&real)), vec![real]);
    }

    #[test]
    fn two_asides_in_a_row_swallow_both_and_nothing_after() {
        let mut gate = AsideGate::default();
        gate.arm();
        gate.arm();
        let chimes = [
            json!({"event":"start-file","playlist_entry_id":1}),
            json!({"event":"property-change","id":1,"name":"media-title","data":"chime 1"}),
            json!({"event":"property-change","id":3,"name":"idle-active","data":false}),
            json!({"event":"property-change","id":6,"name":"path","data":"av://chime1"}),
            json!({"event":"end-file","reason":"stop","playlist_entry_id":1}),
            json!({"event":"start-file","playlist_entry_id":2}),
            json!({"event":"property-change","id":1,"name":"media-title","data":"chime 2"}),
            json!({"event":"property-change","id":6,"name":"path","data":"av://chime2"}),
            json!({"event":"file-loaded"}),
            json!({"event":"playback-restart"}),
            json!({"event":"end-file","reason":"eof","playlist_entry_id":2}),
            json!({"event":"idle"}),
            json!({"event":"property-change","id":1,"name":"media-title"}),
            json!({"event":"property-change","id":3,"name":"idle-active","data":true}),
        ];
        assert_eq!(feed(&mut gate, &chimes), Vec::<Value>::new());
        // The chime is over at its `idle-active: true`: whatever mpv says
        // next is the core's again, a load or not.
        let other = json!({"event":"audio-reconfig"});
        assert!(gate.admit(&other), "the gate stayed shut after the chime's idle");
        let real = json!({"event":"start-file","playlist_entry_id":3});
        let after = json!({"event":"file-loaded"});
        assert_eq!(feed(&mut gate, &[real.clone(), after.clone()]), vec![real, after]);
    }

    #[test]
    fn a_failed_aside_does_not_swallow_the_next_load() {
        let mut gate = AsideGate::default();
        gate.arm();
        gate.disarm();
        assert!(gate.admit(&json!({"event":"start-file","playlist_entry_id":1})));
    }

    /// The gate leaves the startup idle alone; the pump's own `first_idle`
    /// rule still swallows it (see `the_first_observed_idle_is_not_a_stop`).
    #[tokio::test]
    async fn the_initial_idle_is_unaffected() {
        let idle = json!({"event":"property-change","id":3,"name":"idle-active","data":true});
        assert!(AsideGate::default().admit(&idle));

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(format!("{idle}\n").as_bytes()).await.unwrap();
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
    }

    /// Through the pump, the captured sequence end to end: the startup, an
    /// idle chime and its tail produce nothing after the startup's own
    /// event, and a real load after it comes through whole.
    #[tokio::test]
    async fn an_idle_chime_then_a_real_load_through_the_pump() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(format!("{STARTUP}\n").as_bytes()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(-1));
        ipc.gate.lock().unwrap().arm();
        // The fixture's response line answers no pending request: ignored.
        w.write_all(format!("{IDLE_CHIME}\n{IDLE_CHIME_TAIL}\n").as_bytes()).await.unwrap();
        w.write_all(b"{\"event\":\"start-file\",\"playlist_entry_id\":2}\n").await.unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"id\":4,\"name\":\"playlist-pos\",\"data\":0}\n").await.unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"id\":6,\"name\":\"path\",\"data\":\"/m/a.flac\"}\n").await.unwrap();
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
            got.push(ev.expect("an event within 2 s").unwrap());
        }
        // The `path` line feeds the stall journal only: no event.
        assert_eq!(got, vec![Event::FileStarting, Event::TrackChanged(0), Event::PlaybackActive]);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err());
    }

    /// Through the pump, the truncated shape: the swallowed track number is
    /// replayed as a `TrackChanged` right behind the real `FileStarting`.
    #[tokio::test]
    async fn a_swallowed_track_number_reaches_the_core_as_an_event() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(format!("{STARTUP}\n").as_bytes()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(-1));
        ipc.gate.lock().unwrap().arm();
        w.write_all(format!("{IDLE_CHIME}\n").as_bytes()).await.unwrap();
        w.write_all(b"{\"event\":\"start-file\",\"playlist_entry_id\":2}\n").await.unwrap();
        w.write_all(b"{\"event\":\"file-loaded\"}\n").await.unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
            got.push(ev.expect("an event within 2 s").unwrap());
        }
        assert_eq!(got, vec![Event::FileStarting, Event::TrackChanged(0), Event::PlaybackActive]);
    }

    /// A fake mpv answering `get_property` from `answers` (an absent name is
    /// mpv's "property unavailable"), recording every command it receives.
    fn answering(server: UnixStream, answers: Vec<(&'static str, Value)>) -> tokio::task::JoinHandle<Vec<Value>> {
        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["request_id"].as_u64().unwrap();
                let answer = req["command"][1].as_str().and_then(|n| answers.iter().find(|(k, _)| *k == n));
                let resp = match answer {
                    Some((_, data)) => json!({"data": data, "request_id": id, "error": "success"}),
                    None => json!({"request_id": id, "error": "property unavailable"}),
                };
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                seen.push(req["command"].clone());
            }
            seen
        })
    }

    async fn route_with(answers: Vec<(&'static str, Value)>) -> (Result<crate::player::ChimeRoute>, Vec<Value>) {
        route_with_gate(answers, |_| {}).await
    }

    /// `route_with`, the gate first brought to a state by `prepare`.
    async fn route_with_gate(
        answers: Vec<(&'static str, Value)>,
        prepare: impl FnOnce(&mut AsideGate),
    ) -> (Result<crate::player::ChimeRoute>, Vec<Value>) {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        prepare(&mut player.gate());
        let server = answering(server, answers);
        let route = player.chime_route(A_LOCK).await;
        drop(player);
        (route, server.await.unwrap())
    }

    /// What mpv answers while an idle chime plays (measured on mpv 0.37 at
    /// +20/+100/+400 ms after `play_aside`): a real file's answers.
    fn answers_during_a_chime() -> Vec<(&'static str, Value)> {
        vec![
            ("idle-active", json!(false)),
            ("pause", json!(false)),
            ("audio-params", json!({"format": "floatp", "samplerate": 48000, "channel-count": 1})),
        ]
    }

    /// A gate inside an aside, as the chime's `start-file` leaves it.
    fn inside_an_aside(gate: &mut AsideGate) {
        gate.arm();
        assert!(!gate.admit(&json!({"event":"start-file","playlist_entry_id":7})));
    }

    /// A second chime while an idle chime sounds replaces it (`Idle`)
    /// rather than being mixed into it and cut at its end — and mpv, which
    /// would answer like a real file, is not asked.
    ///
    /// **[MUTATION]** the early `sounding_aside` return removed from
    /// `chime_route` → `Live` and three requests: fails.
    #[tokio::test]
    async fn a_chime_while_an_aside_sounds_replaces_it_without_asking_mpv() {
        use crate::player::ChimeRoute;
        let (route, sent) = route_with_gate(answers_during_a_chime(), inside_an_aside).await;
        assert_eq!(route.unwrap(), ChimeRoute::Idle);
        assert_eq!(sent, Vec::<Value>::new(), "no question while the gate is inside");
        // Otherwise the same answers mean a real file playing: mixed.
        let (route, _) = route_with(answers_during_a_chime()).await;
        assert_eq!(route.unwrap(), ChimeRoute::Live { sample_rate: 48000, channels: 1 });
    }

    /// Neither a chime merely armed (its `start-file` may never come) nor a
    /// chime a real load is replacing is taken for idle: an `Idle` there
    /// would send a `replace` cutting the real file.
    #[tokio::test]
    async fn only_a_sounding_aside_short_cuts_the_question() {
        use crate::player::ChimeRoute;
        let live = ChimeRoute::Live { sample_rate: 48000, channels: 1 };
        let (route, sent) = route_with_gate(answers_during_a_chime(), |g| g.arm()).await;
        assert_eq!(route.unwrap(), live, "armed only");
        assert_eq!(sent.len(), 3, "{sent:?}");
        let (route, sent) = route_with_gate(answers_during_a_chime(), |g| {
            inside_an_aside(g);
            g.set_loading(true);
        })
        .await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent, "a real load on its way is never replaced");
        assert_eq!(sent, Vec::<Value>::new(), "{sent:?}");
        // The real file's `start-file` closes both: nothing is an aside.
        let mut gate = AsideGate::default();
        inside_an_aside(&mut gate);
        gate.set_loading(true);
        assert!(!gate.sounding_aside());
        assert!(gate.admit(&json!({"event":"start-file","playlist_entry_id":8})));
        assert!(!gate.sounding_aside());
        gate.set_loading(false);
        assert!(!gate.sounding_aside(), "the real start-file closed the aside");
        // The chime's own idle closes it too.
        let mut gate = AsideGate::default();
        inside_an_aside(&mut gate);
        assert!(gate.sounding_aside());
        assert!(!gate.admit(&json!({"event":"property-change","id":3,"name":"idle-active","data":true})));
        assert!(!gate.sounding_aside());
    }

    /// `play` and `load_list` tell the gate a real load is on its way, so a
    /// chime asked for before its `start-file` is not sent as an aside that
    /// would replace it; a refused load tells it the opposite.
    #[tokio::test]
    async fn a_real_load_ends_the_aside_shortcut_at_once() {
        use crate::player::Player;
        for refused in [false, true] {
            for list in [false, true] {
                let (client, server) = UnixStream::pair().unwrap();
                let (tx, _rx) = mpsc::channel(16);
                let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
                inside_an_aside(&mut player.gate());
                let server = tokio::spawn(async move {
                    let (r, mut w) = server.into_split();
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let load = matches!(req["command"][0].as_str(), Some("loadfile" | "loadlist"));
                        let error = if refused && load { "error running command" } else { "success" };
                        let resp = json!({"request_id": req["request_id"], "error": error});
                        w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                    }
                });
                let res = if list { player.load_list("/m/l.m3u", None).await } else { player.play("/m/a.flac").await };
                assert_eq!(res.is_err(), refused);
                assert_eq!(player.gate().sounding_aside(), refused, "refused {refused}, list {list}");
                drop(player);
                server.await.unwrap();
            }
        }
    }

    /// A `play` from idle, then a lock change before that file's
    /// `start-file`: mpv still answers `idle-active` true, but an `Idle`
    /// would send a chime replacing the load just asked for. `Silent`, and
    /// mpv not asked; the same when the load is sent while mpv is asked.
    ///
    /// **[MUTATION]** the `loading` checks removed from `chime_route` →
    /// `Idle`: fails.
    #[tokio::test]
    async fn a_load_in_flight_is_never_replaced_by_a_chime() {
        use crate::player::{ChimeRoute, Player};
        let (route, sent) = route_with_gate(vec![("idle-active", json!(true))], |g| g.set_loading(true)).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        assert_eq!(sent, Vec::<Value>::new(), "no question while a load is in flight");

        // The load sent while mpv is being asked: the answer is re-read.
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let gate = player.ipc.gate.clone();
        let server = tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                gate.lock().unwrap().set_loading(true);
                let resp = json!({"data": true, "request_id": req["request_id"], "error": "success"});
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
            }
        });
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Silent);
        drop(player);
        server.await.unwrap();
    }

    /// Armed when asked, inside once mpv has answered: the chime's
    /// `start-file`, read before the answer, makes the route `Idle`.
    #[tokio::test]
    async fn a_chime_starting_while_mpv_is_asked_is_idle() {
        use crate::player::{ChimeRoute, Player};
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        player.gate().arm();
        let server = tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                if seen.is_empty() {
                    w.write_all(b"{\"event\":\"start-file\",\"playlist_entry_id\":7}\n").await.unwrap();
                }
                let answer = answers_during_a_chime().into_iter().find(|(k, _)| req["command"][1] == json!(k));
                let resp = json!({"data": answer.unwrap().1, "request_id": req["request_id"], "error": "success"});
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                seen.push(req["command"].clone());
            }
            seen
        });
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Idle);
        drop(player);
        assert_eq!(server.await.unwrap().len(), 3, "armed only when asked: mpv was asked");
    }

    #[tokio::test]
    async fn route_is_silent_while_loading() {
        use crate::player::ChimeRoute;
        // Paused but still loading: no audio parameters, nothing more asked.
        let (route, sent) = route_with(vec![("idle-active", json!(false)), ("pause", json!(true))]).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        assert_eq!(sent.len(), 3, "{sent:?}");
        // Loading: no audio parameters yet, as an error or as a `null`.
        let (route, _) = route_with(vec![("idle-active", json!(false)), ("pause", json!(false))]).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        let (route, _) =
            route_with(vec![("idle-active", json!(false)), ("pause", json!(false)), ("audio-params", Value::Null)]).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        let (route, _) = route_with(vec![
            ("idle-active", json!(false)),
            ("pause", json!(false)),
            ("audio-params", json!({"format": "floatp", "samplerate": 44100})),
        ])
        .await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        let (route, sent) = route_with(vec![
            ("idle-active", json!(false)),
            ("pause", json!(false)),
            ("audio-params", json!({"format": "floatp", "samplerate": 44100, "channel-count": 2})),
        ])
        .await;
        assert_eq!(route.unwrap(), ChimeRoute::Live { sample_rate: 44100, channels: 2 });
        assert_eq!(
            sent,
            vec![
                json!(["get_property", "idle-active"]),
                json!(["get_property", "pause"]),
                json!(["get_property", "audio-params"]),
            ]
        );
        let (route, sent) = route_with(vec![("idle-active", json!(true))]).await;
        assert_eq!(route.unwrap(), ChimeRoute::Idle);
        assert_eq!(sent, vec![json!(["get_property", "idle-active"])], "Idle after a single request");
        // mpv unreachable on the first question: a failure, not a route.
        let (route, _) = route_with(vec![]).await;
        assert!(route.is_err());
    }

    /// The chime of `A_LOCK`'s length: 0.74 s.
    const A_LOCK: std::time::Duration = std::time::Duration::from_millis(740);

    /// What mpv answers for a paused file, the paused spike's values (kf48
    /// cycle 0: 48 kHz stereo, paused at 4.416 of 150 s, seekable).
    fn paused_answers(position: Value, duration: Value, seekable: Value) -> Vec<(&'static str, Value)> {
        let mut answers = vec![
            ("idle-active", json!(false)),
            ("pause", json!(true)),
            (
                "audio-params",
                json!({"samplerate":48000,"channel-count":2,"channels":"stereo","hr-channels":"stereo","format":"float"}),
            ),
        ];
        for (name, value) in [("time-pos", position), ("duration", duration), ("seekable", seekable)] {
            if !value.is_null() {
                answers.push((name, value));
            }
        }
        answers
    }

    /// **[MUTATION]** each branch of the paused mapping: `resume_at` taken
    /// from a non-seekable stream, the near-end guard dropped or applied to
    /// a stream, or a seekable file without a position let through.
    #[tokio::test]
    async fn the_route_of_a_paused_file_says_where_to_put_it_back() {
        use crate::player::ChimeRoute;
        let paused = |resume_at| ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at };

        let (route, sent) = route_with(paused_answers(json!(4.416), json!(150.0), json!(true))).await;
        assert_eq!(route.unwrap(), paused(Some(4.416)));
        let get = |name: &str| json!(["get_property", name]);
        assert_eq!(
            sent,
            ["idle-active", "pause", "audio-params", "time-pos", "duration", "seekable"].map(get).to_vec(),
            "the paused spike's order"
        );

        // A stream (`seekable` false): no position to go back to, and its
        // `duration`, the cache end, never silences it (spike: 1.386667).
        let (route, _) = route_with(paused_answers(json!(1.2), json!(1.386667), json!(false))).await;
        assert_eq!(route.unwrap(), paused(None));
        // `seekable` unavailable, or not a boolean, says nothing: a file may
        // be taken for a stream there, not put back and not guarded against
        // its end. Silent.
        let (route, _) = route_with(paused_answers(json!(1.2), json!(150.0), Value::Null)).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent, "seekable unavailable");
        let (route, _) = route_with(paused_answers(json!(1.2), json!(150.0), json!("yes"))).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent, "seekable not a boolean");

        // The near-end guard: 0.74 s + 1 s must remain.
        let (route, _) = route_with(paused_answers(json!(148.0), json!(149.73), json!(true))).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent, "1.73 s left");
        let (route, _) = route_with(paused_answers(json!(148.0), json!(149.75), json!(true))).await;
        assert_eq!(route.unwrap(), paused(Some(148.0)), "1.75 s left");
        // The guard follows the chime's length.
        let (client_route, _) = {
            use crate::player::Player;
            let (client, server) = UnixStream::pair().unwrap();
            let (tx, _rx) = mpsc::channel(16);
            let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
            let server = answering(server, paused_answers(json!(148.0), json!(149.75), json!(true)));
            let route = player.chime_route(std::time::Duration::from_millis(1630)).await;
            drop(player);
            (route, server.await.unwrap())
        };
        assert_eq!(client_route.unwrap(), ChimeRoute::Silent, "a 1.63 s chime needs 2.63 s");
        // No duration known: no guard.
        let (route, _) = route_with(paused_answers(json!(148.0), Value::Null, json!(true))).await;
        assert_eq!(route.unwrap(), paused(Some(148.0)));
        // Seekable but no position: nowhere to put it back.
        let (route, _) = route_with(paused_answers(Value::Null, json!(150.0), json!(true))).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
    }

    /// A second lock change after a paused chime, still paused: the pause
    /// point is the one the gate kept, not `time-pos`, which right after a
    /// restore read 0.17 s short (see `end_paused_chime`). **[MUTATION]**
    /// the frozen position ignored → the second chime restores 0.17 s early.
    /// Also the near-end guard reads it.
    #[tokio::test]
    async fn after_a_restore_the_route_puts_the_file_back_at_the_kept_pause_point() {
        use crate::player::ChimeRoute;
        let kept = |g: &mut AsideGate| {
            g.begin_paused_chime(Progress { position_s: Some(4.416), duration_s: Some(150.0) });
            g.end_paused_chime(true);
        };
        let (route, _) = route_with_gate(paused_answers(json!(4.25), json!(150.0), json!(true)), kept).await;
        assert_eq!(route.unwrap(), ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: Some(4.416) });
        // A guard on the kept point: 1.73 s left from 4.416, not from 4.25.
        let (route, _) = route_with_gate(paused_answers(json!(4.25), json!(6.146), json!(true)), kept).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        // A stream keeps no position to go back to.
        let (route, _) = route_with_gate(paused_answers(json!(4.25), json!(5.0), json!(false)), kept).await;
        assert_eq!(route.unwrap(), ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: None });
    }

    /// **[MUTATION]** the `in_paused_chime` checks removed from
    /// `chime_route` → mpv, unpaused for the chime, answers like a playing
    /// file and the route is `Live`: a chime mixed over the music at full
    /// gain.
    #[tokio::test]
    async fn a_paused_chime_sounding_is_silent_without_a_question() {
        use crate::player::ChimeRoute;
        let (route, sent) = route_with_gate(answers_during_a_chime(), |g| g.begin_paused_chime(Progress::default())).await;
        assert_eq!(route.unwrap(), ChimeRoute::Silent);
        assert_eq!(sent, Vec::<Value>::new());
        // And once it ended, mpv is asked again.
        let (route, _) = route_with_gate(answers_during_a_chime(), |g| {
            g.begin_paused_chime(Progress::default());
            g.end_paused_chime(true);
        })
        .await;
        assert_eq!(route.unwrap(), ChimeRoute::Live { sample_rate: 48000, channels: 1 });
    }

    /// A paused chime, from the chain write to just before the re-pause.
    /// Captured on mpv 0.37.0 by the lock-sound 9b spike, 2026-11-17 (kf48,
    /// cycle 0, the recommended sequence; all 20 cycles have this shape):
    /// set af (16), flush seek (17), unpause (18), the spike's own
    /// `get time-pos` mid-chime (19).
    const PAUSED_CHIME_SOUNDING: &str = r#"{"request_id":16,"error":"success"}
{"event":"audio-reconfig"}
{"data":null,"request_id":17,"error":"success"}
{"request_id":18,"error":"success"}
{"event":"seek"}
{"event":"playback-restart"}
{"data":6.304000,"request_id":19,"error":"success"}"#;

    /// Same capture: re-pause (20), set af (21), restore seek (22), the
    /// spike's `get time-pos` (23).
    const PAUSED_CHIME_ENDING: &str = r#"{"request_id":20,"error":"success"}
{"request_id":21,"error":"success"}
{"event":"audio-reconfig"}
{"data":null,"request_id":22,"error":"success"}
{"data":4.416000,"request_id":23,"error":"success"}
{"event":"seek"}"#;

    /// Same capture: the user's resume (`cycle pause`, 24), then the restore
    /// seek's `playback-restart`, 0.29 s later.
    const PAUSED_CHIME_RESUME: &str = r#"{"data":null,"request_id":24,"error":"success"}
{"event":"playback-restart"}"#;

    /// A non-seekable stream's paused chime (the spike's st48, 20 cycles):
    /// two `audio-reconfig`, nothing else, nothing after the resume. The
    /// spike reported the shape, not the lines: these are mpv's own form
    /// of that event.
    const PAUSED_STREAM_CHIME: &str = r#"{"event":"audio-reconfig"}
{"event":"audio-reconfig"}"#;

    /// The lines the pump turns into events (the rest it drops).
    fn reaching_the_core(lines: Vec<Value>) -> Vec<Value> {
        let kinds = ["property-change", "file-loaded", "start-file", "playback-restart"];
        lines.into_iter().filter(|l| kinds.iter().any(|k| l["event"] == json!(k))).collect()
    }

    /// **[MUTATION]** the `playback-restart` swallowing removed, or the
    /// pending restore not set at the end → the flush's or the restore's
    /// `PlaybackRestarted` reaches the core. And exactly one is swallowed
    /// after the chime: the user's next seek gets through.
    #[test]
    fn a_paused_chime_reaches_the_core_as_nothing() {
        let mut gate = AsideGate::default();
        let startup = lines_of(STARTUP);
        assert_eq!(feed(&mut gate, &startup), startup);
        gate.begin_paused_chime(Progress { position_s: Some(4.416), duration_s: Some(150.0) });
        assert_eq!(reaching_the_core(feed(&mut gate, &lines_of(PAUSED_CHIME_SOUNDING))), Vec::<Value>::new());
        assert_eq!(reaching_the_core(feed(&mut gate, &lines_of(PAUSED_CHIME_ENDING))), Vec::<Value>::new());
        gate.end_paused_chime(true);
        let pause_point = Some(Progress { position_s: Some(4.416), duration_s: Some(150.0) });
        assert_eq!(gate.frozen(), pause_point, "frozen after the chime");
        assert_eq!(reaching_the_core(feed(&mut gate, &lines_of(PAUSED_CHIME_RESUME))), Vec::<Value>::new());
        assert_eq!(gate.frozen(), pause_point, "still the pause's until the user moves (`thaw`)");
        let later = json!({"event":"playback-restart"});
        assert!(gate.admit(&later), "only the restore's playback-restart is swallowed");
    }

    /// **[MUTATION]** `restore_pending` set whatever `restore_sent` says →
    /// the next real `playback-restart` (a seek after the resume) is lost.
    #[test]
    fn a_stream_chime_leaves_nothing_pending() {
        let mut gate = AsideGate::default();
        gate.begin_paused_chime(Progress::default());
        assert_eq!(reaching_the_core(feed(&mut gate, &lines_of(PAUSED_STREAM_CHIME))), Vec::<Value>::new());
        gate.end_paused_chime(false);
        assert!(gate.admit(&json!({"event":"playback-restart"})));
    }

    /// The user changes source during a paused chime: the new file's
    /// `start-file` passes, ends the chime for the gate, and that file's own
    /// `playback-restart` passes too. **[MUTATION]** the reset at
    /// `start-file` removed → its `playback-restart` is swallowed.
    #[test]
    fn a_real_file_started_during_a_paused_chime_passes_through() {
        let mut gate = AsideGate::default();
        gate.begin_paused_chime(Progress { position_s: Some(4.416), duration_s: Some(150.0) });
        feed(&mut gate, &lines_of(PAUSED_CHIME_SOUNDING));
        let start = json!({"event":"start-file","playlist_entry_id":2});
        assert!(gate.admit(&start));
        assert!(!gate.in_paused_chime());
        assert_eq!(gate.frozen(), None, "the new file's progress is mpv's");
        assert!(gate.admit(&json!({"event":"playback-restart"})));

        // The same after the chime, before the restore's `playback-restart`
        // came: the new file's is real.
        let mut gate = AsideGate::default();
        gate.begin_paused_chime(Progress::default());
        gate.end_paused_chime(true);
        assert!(gate.admit(&start));
        assert!(gate.admit(&json!({"event":"playback-restart"})));
    }

    /// mpv went on to the next file by itself during a paused chime: the
    /// core ends the chime in place after that `start-file` (paused again,
    /// plain chain, no seek). Nothing is left pending: the new file's
    /// `playback-restart` and its progress are mpv's, and so are those of
    /// the user's next resume.
    #[test]
    fn a_paused_chime_ended_after_mpvs_own_file_start_leaves_nothing_pending() {
        let mut gate = AsideGate::default();
        gate.begin_paused_chime(Progress { position_s: Some(148.5), duration_s: None });
        assert!(gate.admit(&json!({"event":"start-file","playlist_entry_id":2})));
        gate.end_paused_chime(false);
        assert!(!gate.in_paused_chime());
        assert_eq!(gate.frozen(), None);
        assert!(gate.admit(&json!({"event":"playback-restart"})), "the new file's own");
        assert!(gate.admit(&json!({"event":"playback-restart"})), "the user's resume");
    }

    /// A second chime keeps the first one's frozen progress: the file was
    /// paused there, whatever `time-pos` says mid-chime.
    #[test]
    fn a_second_paused_chime_keeps_the_first_pause_point() {
        let mut gate = AsideGate::default();
        let first = Progress { position_s: Some(4.416), duration_s: Some(150.0) };
        gate.begin_paused_chime(first);
        gate.begin_paused_chime(Progress { position_s: Some(6.304), duration_s: Some(150.0) });
        assert_eq!(gate.frozen(), Some(first));
        gate.end_paused_chime(true);
        assert_eq!(gate.frozen(), Some(first));
    }

    /// After the chime, the frozen progress lasts until the user moves
    /// playback (`thaw`) — never while a chime sounds — and a stream's
    /// ends with the chime (no restore to wait for). **[MUTATION]** `thaw`
    /// emptied → a resume whose `playback-restart` never comes keeps the bar
    /// still while the music plays; `thaw` unconditional → the chime moves
    /// the bar.
    #[test]
    fn the_frozen_progress_ends_with_the_users_next_move() {
        let mut gate = AsideGate::default();
        let first = Progress { position_s: Some(4.416), duration_s: Some(150.0) };
        gate.begin_paused_chime(first);
        gate.thaw();
        assert_eq!(gate.frozen(), Some(first), "a chime sounds: nothing thaws it");
        gate.end_paused_chime(true);
        gate.thaw();
        assert_eq!(gate.frozen(), None);

        gate.begin_paused_chime(first);
        gate.end_paused_chime(false);
        assert_eq!(gate.frozen(), None, "a stream: nothing to wait for");
    }

    /// Every transport call thaws: the user's resume among them.
    #[tokio::test]
    async fn a_resume_thaws_the_progress() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let _server = scripted(server, vec![("time-pos", json!(4.5)), ("duration", json!(150.0))], &[], None);
        let first = Progress { position_s: Some(4.416), duration_s: Some(150.0) };
        player.gate().begin_paused_chime(first);
        player.gate().end_paused_chime(true);
        assert_eq!(player.progress().await.unwrap(), first);
        player.toggle_pause().await.unwrap();
        assert_eq!(player.progress().await.unwrap(), Progress { position_s: Some(4.5), duration_s: Some(150.0) });
    }

    /// Through the pump, the captured chime end to end: nothing reaches the
    /// core, then a real load does.
    #[tokio::test]
    async fn a_paused_chime_through_the_pump() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        ipc.gate.lock().unwrap().begin_paused_chime(Progress::default());
        w.write_all(format!("{PAUSED_CHIME_SOUNDING}\n{PAUSED_CHIME_ENDING}\n").as_bytes()).await.unwrap();
        // The pump has read them once the counter moved twice.
        let mut reconfigs = ipc.reconfigs.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(2), reconfigs.wait_for(|n| *n == 2)).await.unwrap().unwrap();
        ipc.gate.lock().unwrap().end_paused_chime(true);
        w.write_all(format!("{PAUSED_CHIME_RESUME}\n").as_bytes()).await.unwrap();
        w.write_all(b"{\"event\":\"start-file\",\"playlist_entry_id\":2}\n").await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert_eq!(ev.unwrap().unwrap(), Event::FileStarting, "nothing of the chime came before");
    }

    /// A fake mpv answering every command with success (`data` from
    /// `answers` for a `get_property`), and writing `after_af` events that
    /// long after each `set_property af` response. Records each command with
    /// when it arrived.
    #[allow(clippy::type_complexity)]
    fn scripted(
        server: UnixStream,
        answers: Vec<(&'static str, Value)>,
        refuse: &'static [&'static str],
        after_af: Option<std::time::Duration>,
    ) -> tokio::task::JoinHandle<Vec<(std::time::Instant, Value)>> {
        tokio::spawn(async move {
            let (r, w) = server.into_split();
            let w = Arc::new(Mutex::new(w));
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let cmd = req["command"].clone();
                let id = req["request_id"].clone();
                let name = cmd[1].as_str().unwrap_or_default();
                // A property's name for `set_property`, the command's own otherwise.
                let key = if cmd[0] == json!("set_property") { name } else { cmd[0].as_str().unwrap_or_default() };
                let resp = if cmd[0] == json!("get_property") {
                    match answers.iter().find(|(k, _)| *k == name) {
                        Some((_, data)) => json!({"data": data, "request_id": id, "error": "success"}),
                        None => json!({"request_id": id, "error": "property unavailable"}),
                    }
                } else if refuse.contains(&key) {
                    json!({"request_id": id, "error": "error running command"})
                } else {
                    json!({"data": null, "request_id": id, "error": "success"})
                };
                w.lock().await.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                if cmd[0] == json!("set_property")
                    && name == "af"
                    && let Some(delay) = after_af
                {
                    let w = w.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let _ = w.lock().await.write_all(b"{\"event\":\"audio-reconfig\"}\n").await;
                    });
                }
                seen.push((std::time::Instant::now(), cmd));
            }
            seen
        })
    }

    fn commands(seen: &[(std::time::Instant, Value)]) -> Vec<Value> {
        seen.iter().map(|(_, c)| c.clone()).collect()
    }

    /// An mpv that answers its first `answered` commands at once (a
    /// `get_property` from `answers`, anything else as a success), then stops
    /// answering: the rest are read and recorded, and answered `late` after
    /// they came, or never. Records every command it read, in order.
    fn stalling(
        server: UnixStream,
        answers: Vec<(&'static str, Value)>,
        answered: usize,
        late: Option<std::time::Duration>,
    ) -> (tokio::task::JoinHandle<()>, Arc<std::sync::Mutex<Vec<Value>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        let task = tokio::spawn(async move {
            let (r, w) = server.into_split();
            let w = Arc::new(Mutex::new(w));
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let cmd = req["command"].clone();
                let id = req["request_id"].clone();
                let n = {
                    let mut seen = record.lock().unwrap();
                    seen.push(cmd.clone());
                    seen.len()
                };
                let resp = match answers.iter().find(|(k, _)| cmd[0] == json!("get_property") && cmd[1] == json!(k)) {
                    Some((_, data)) => json!({"data": data, "request_id": id, "error": "success"}),
                    None if cmd[0] == json!("get_property") => json!({"request_id": id, "error": "property unavailable"}),
                    None => json!({"data": null, "request_id": id, "error": "success"}),
                };
                let delay = if n <= answered { Some(std::time::Duration::ZERO) } else { late };
                if let Some(delay) = delay {
                    let w = w.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let _ = w.lock().await.write_all(format!("{resp}\n").as_bytes()).await;
                    });
                }
            }
        });
        (task, seen)
    }

    fn stalling_player(
        answers: Vec<(&'static str, Value)>,
        answered: usize,
        late: Option<std::time::Duration>,
    ) -> (MpvPlayer, Arc<std::sync::Mutex<Vec<Value>>>) {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let (_task, seen) = stalling(server, answers, answered, late);
        (player, seen)
    }

    /// Slack over a budget for the test machine's scheduling.
    const SLACK: std::time::Duration = std::time::Duration::from_millis(400);

    /// mpv stops answering at the start's first question: the start gives
    /// up within its budget — not 5 s per command — before touching
    /// anything: no chain, no unpause, the gate as it was.
    /// **[MUTATION]** the start's budget replaced by the 5 s per command →
    /// about 10 s.
    #[tokio::test]
    async fn a_paused_chime_start_gives_up_within_its_budget_before_touching_anything() {
        use crate::player::Player;
        let (player, seen) = stalling_player(vec![], 0, None);
        let began = std::time::Instant::now();
        let result = player.sound_while_paused("eq,@chime:x", Some(4.416)).await;
        let took = began.elapsed();
        assert!(result.unwrap_err().is::<CommandTimeout>());
        assert!(took >= PAUSED_CHIME_START_BUDGET && took < PAUSED_CHIME_START_BUDGET + SLACK, "{took:?}");
        assert_eq!(*seen.lock().unwrap(), vec![json!(["get_property", "time-pos"])], "nothing more sent");
        assert!(!player.gate().in_paused_chime());
        assert_eq!(player.gate().frozen(), None);
    }

    /// mpv stops answering after the chain and the flush seek were sent:
    /// the start gives up within its budget without the unpause; the end
    /// that follows (the core calls it at once) gives up within its own and
    /// still **writes** the pause, the plain chain and the restore seek —
    /// in that order, so an mpv that answers late ends paused, on the
    /// plain chain, put back — and closes the gate, the restore's
    /// `playback-restart` still to swallow and the pause's progress kept.
    /// **[MUTATION]** the end's steps not written past the budget (`act_until`
    /// in place of `command_until`) → the seek, then the chain, missing;
    /// an unanswered seek counted as not sent → nothing to swallow.
    #[tokio::test]
    async fn a_paused_chime_cut_short_by_a_silent_mpv_ends_within_its_budget_with_every_step_written() {
        use crate::player::Player;
        let answers = vec![("time-pos", json!(4.416)), ("duration", json!(150.0))];
        let (player, seen) = stalling_player(answers, 3, None);
        let began = std::time::Instant::now();
        assert!(player.sound_while_paused("eq,@chime:x", Some(4.416)).await.is_err());
        assert!(began.elapsed() < PAUSED_CHIME_START_BUDGET + SLACK, "{:?}", began.elapsed());
        assert!(player.gate().in_paused_chime());
        let began = std::time::Instant::now();
        assert!(player.end_sound_while_paused("eq", Some(4.416)).await.is_err());
        let took = began.elapsed();
        assert!(took < PAUSED_CHIME_END_BUDGET + SLACK, "{took:?}");
        // Give the server the time to read the last writes.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                json!(["get_property", "time-pos"]),
                json!(["get_property", "duration"]),
                json!(["set_property", "af", "eq,@chime:x"]),
                json!(["seek", 4.416, "absolute+keyframes"]),
                json!(["set_property", "pause", true]),
                json!(["set_property", "af", "eq"]),
                json!(["seek", 4.416, "absolute+exact"]),
            ],
            "no unpause; the end written whole, in order"
        );
        let mut gate = player.gate();
        assert!(!gate.in_paused_chime());
        assert_eq!(gate.frozen(), Some(Progress { position_s: Some(4.416), duration_s: Some(150.0) }));
        assert!(!gate.admit(&json!({"event":"playback-restart"})), "the restore's, should mpv apply it late");
        assert!(gate.admit(&json!({"event":"playback-restart"})));
    }

    /// The end, mpv silent from its first command: within its budget, the
    /// chain wait cut to what is left. A stream: no seek.
    #[tokio::test]
    async fn a_paused_chime_end_on_a_silent_mpv_stays_within_its_budget() {
        use crate::player::Player;
        let (player, seen) = stalling_player(vec![], 0, None);
        player.gate().begin_paused_chime(Progress::default());
        let began = std::time::Instant::now();
        assert!(player.end_sound_while_paused("", None).await.is_err());
        assert!(began.elapsed() < PAUSED_CHIME_END_BUDGET + SLACK, "{:?}", began.elapsed());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(*seen.lock().unwrap(), vec![json!(["set_property", "pause", true]), json!(["set_property", "af", ""])]);
        assert!(!player.gate().in_paused_chime());
        assert!(player.gate().admit(&json!({"event":"playback-restart"})), "no seek: nothing pending");
    }

    /// The route's questions share one budget; past it, `seekable` is not
    /// asked and a paused file gets no chime — never one without its
    /// near-end guard because `duration` came too late.
    /// **[MUTATION]** the route's budget off → 5 s per question.
    #[tokio::test]
    async fn the_route_on_a_silent_mpv_gives_up_within_its_budget_and_stays_silent() {
        use crate::player::{ChimeRoute, Player};
        let (player, seen) = stalling_player(vec![], 0, None);
        let began = std::time::Instant::now();
        assert!(player.chime_route(A_LOCK).await.unwrap_err().is::<CommandTimeout>());
        assert!(began.elapsed() < CHIME_ROUTE_BUDGET + SLACK, "{:?}", began.elapsed());
        assert_eq!(*seen.lock().unwrap(), vec![json!(["get_property", "idle-active"])]);

        // mpv stalls at `duration`.
        let answers = paused_answers(json!(4.416), json!(150.0), json!(true));
        let (player, seen) = stalling_player(answers, 4, None);
        let began = std::time::Instant::now();
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Silent);
        assert!(began.elapsed() < CHIME_ROUTE_BUDGET + SLACK, "{:?}", began.elapsed());
        assert!(!seen.lock().unwrap().contains(&json!(["get_property", "seekable"])), "{:?}", seen.lock().unwrap());
    }

    /// Answers that come back after their command gave up answer nothing,
    /// and mpv's next answers reach their own commands: a late mpv leaves
    /// the connection usable.
    #[tokio::test]
    async fn late_answers_after_a_given_up_chime_leave_the_connection_usable() {
        use crate::player::Player;
        let (player, _seen) = stalling_player(vec![("time-pos", json!(4.416))], 0, Some(std::time::Duration::from_secs(2)));
        assert!(player.sound_while_paused("eq,@chime:x", Some(4.416)).await.is_err());
        // Answered 2 s after it came, within the 5 s of an ordinary command,
        // after the start's late answer has come back.
        let began = std::time::Instant::now();
        player.set_volume(40).await.expect("the connection still works");
        assert!(began.elapsed() >= std::time::Duration::from_millis(1500), "answered late, as scripted");
    }

    /// The spike's sequence (kf48): the pause's progress read and frozen,
    /// the chain, the flush seek `absolute+keyframes` at the number as given,
    /// the unpause; and `progress` asks mpv nothing until the end. A stream:
    /// no flush. **[MUTATION]** the flush seek dropped, or sent `+exact`.
    #[tokio::test]
    async fn sound_while_paused_sends_the_measured_sequence() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let server = scripted(server, vec![("time-pos", json!(4.416)), ("duration", json!(150.0))], &[], None);
        player.sound_while_paused("eqchain,@chime:x", Some(4.416)).await.unwrap();
        let frozen = Progress { position_s: Some(4.416), duration_s: Some(150.0) };
        assert_eq!(player.progress().await.unwrap(), frozen);
        // A second chime: no new progress read, the chain again.
        player.sound_while_paused("eqchain,@chime:y", Some(4.416)).await.unwrap();
        assert_eq!(player.progress().await.unwrap(), frozen);
        drop(player);
        assert_eq!(
            commands(&server.await.unwrap()),
            vec![
                json!(["get_property", "time-pos"]),
                json!(["get_property", "duration"]),
                json!(["set_property", "af", "eqchain,@chime:x"]),
                json!(["seek", 4.416, "absolute+keyframes"]),
                json!(["set_property", "pause", false]),
                json!(["set_property", "af", "eqchain,@chime:y"]),
                json!(["seek", 4.416, "absolute+keyframes"]),
                json!(["set_property", "pause", false]),
            ]
        );

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let server = scripted(server, vec![("time-pos", json!(12.0))], &[], None);
        player.sound_while_paused("@chime:x", None).await.unwrap();
        assert!(player.gate().in_paused_chime());
        drop(player);
        assert_eq!(
            commands(&server.await.unwrap()),
            vec![
                json!(["get_property", "time-pos"]),
                json!(["get_property", "duration"]),
                json!(["set_property", "af", "@chime:x"]),
                json!(["set_property", "pause", false]),
            ]
        );
    }

    /// The end: re-pause, the plain chain, **its `audio-reconfig` awaited**,
    /// then the restore seek `absolute+exact`; the gate left waiting for the
    /// restore's `playback-restart`, `progress` still frozen.
    /// **[MUTATION]** the wait removed → the seek arrives with the af write,
    /// before mpv rebuilt the chain (the spike's lost restore).
    #[tokio::test]
    async fn the_restore_seek_waits_for_the_chain_rebuild() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        player.gate().begin_paused_chime(Progress::default());
        let server = scripted(server, vec![], &[], Some(std::time::Duration::from_millis(300)));
        player.end_sound_while_paused("eqchain", Some(4.416)).await.unwrap();
        assert!(!player.gate().in_paused_chime());
        assert_eq!(player.gate().frozen(), Some(Progress::default()));
        assert!(!player.gate().admit(&json!({"event":"playback-restart"})), "the restore's is still to come");
        assert_eq!(player.gate().frozen(), Some(Progress::default()), "still paused: frozen until the user moves");
        drop(player);
        let seen = server.await.unwrap();
        assert_eq!(
            commands(&seen),
            vec![
                json!(["set_property", "pause", true]),
                json!(["set_property", "af", "eqchain"]),
                json!(["seek", 4.416, "absolute+exact"]),
            ]
        );
        let waited = seen[2].0 - seen[1].0;
        assert!(waited >= std::time::Duration::from_millis(250), "the seek did not wait for the rebuild: {waited:?}");
    }

    /// No `audio-reconfig` at all: the seek still goes, after the bound.
    #[tokio::test]
    async fn the_restore_wait_is_bounded() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        player.gate().begin_paused_chime(Progress::default());
        let server = scripted(server, vec![], &[], None);
        let started = std::time::Instant::now();
        player.end_sound_while_paused("eqchain", Some(4.416)).await.unwrap();
        let took = started.elapsed();
        drop(player);
        assert_eq!(commands(&server.await.unwrap()).len(), 3);
        assert!(took >= RESTORE_RECONFIG_WAIT && took < RESTORE_RECONFIG_WAIT * 3, "{took:?}");
    }

    /// Every step is tried: a refused re-pause still writes the plain chain
    /// (the music must never stay at gain 0) and the first error comes
    /// back; a stream has no seek and nothing pending; a refused seek leaves
    /// nothing pending either. **[MUTATION]** `?` on the pause → the chain
    /// is never written.
    #[tokio::test]
    async fn the_end_tries_every_step() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        player.gate().begin_paused_chime(Progress::default());
        let server = scripted(server, vec![], &["pause"], Some(std::time::Duration::ZERO));
        assert!(player.end_sound_while_paused("eqchain", None).await.is_err());
        assert!(!player.gate().in_paused_chime());
        assert!(player.gate().admit(&json!({"event":"playback-restart"})), "no seek: nothing pending");
        drop(player);
        assert_eq!(
            commands(&server.await.unwrap()),
            vec![json!(["set_property", "pause", true]), json!(["set_property", "af", "eqchain"])]
        );

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        player.gate().begin_paused_chime(Progress::default());
        let _server = scripted(server, vec![], &["seek"], Some(std::time::Duration::ZERO));
        assert!(player.end_sound_while_paused("eqchain", Some(4.416)).await.is_err());
        assert!(player.gate().admit(&json!({"event":"playback-restart"})), "a refused seek leaves nothing pending");
    }

    /// The form measured by the lock-sound spike on mpv 0.37: the file-local
    /// options keep a global `pause` and `loop-file` from touching the chime.
    #[tokio::test]
    async fn play_aside_sends_the_named_loadfile() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let server = tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["request_id"].as_u64().unwrap();
                let resp = json!({"data": {"playlist_entry_id": 1}, "request_id": id, "error": "success"});
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                seen.push(req);
            }
            seen
        });
        let uri = "av://lavfi:aevalsrc=exprs='sin(2*PI*440*t)':s=48000:d=0.5";
        player.play_aside(uri, None).await.unwrap();
        drop(player);
        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(
            seen[0]["command"],
            json!({"name": "loadfile", "url": uri, "flags": "replace", "options": "loop-file=no,pause=no"})
        );
        assert!(seen[0]["request_id"].is_u64(), "{}", seen[0]);
    }

    /// mpv refusing the load: the error comes back, and the next real load's
    /// `start-file` is not taken for the chime's.
    #[tokio::test]
    async fn a_refused_play_aside_leaves_the_gate_open() {
        use crate::player::Player;
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let player = MpvPlayer { ipc: MpvIpc::from_stream(client, tx), chime_dir: std::path::PathBuf::new() };
        let (r, mut w) = server.into_split();
        let mut lines = BufReader::new(r).lines();
        let answer = async {
            let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let resp = json!({"request_id": req["request_id"], "error": "error running command"});
            w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
        };
        let (res, ()) = tokio::join!(player.play_aside("av://lavfi:anullsrc", None), answer);
        assert!(res.is_err());
        w.write_all(b"{\"event\":\"start-file\",\"playlist_entry_id\":1}\n").await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(matches!(ev, Ok(Some(Event::FileStarting))), "{ev:?}");
    }

    /// `start` on a wrapper script a test has just written. Retried on
    /// `ETXTBSY` only: another test thread forking while the script's file
    /// was open for writing holds a copy of that descriptor until its own
    /// `exec`, and running the script in that window fails ("Text file
    /// busy") — a race of parallel tests, never of the device.
    async fn start_test_mpv(mpv: &Path, d: &Path, af: &str, tx: mpsc::Sender<Event>) -> (MpvPlayer, tokio::process::Child) {
        for _ in 0..50 {
            match start(mpv.to_str().unwrap(), &d.join("mpv.sock"), "/dev/null", AUDIO_BUFFER_DEFAULT, READAHEAD_DEFAULT, af, tx.clone()).await {
                Ok(started) => return started,
                Err(e) if e.chain().any(|c| c.downcast_ref::<std::io::Error>().and_then(std::io::Error::raw_os_error) == Some(26)) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => panic!("starting mpv: {e:#}"),
            }
        }
        panic!("starting mpv: text file still busy after 50 attempts");
    }

    fn mpv_missing() -> bool {
        std::process::Command::new("mpv").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true)
    }

    fn wrapper(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Amplitude of the `freq` component of `x` (Goertzel), as a fraction of
    /// full scale.
    fn tone_amplitude(x: &[f64], freq: f64, rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        let (mut s1, mut s2) = (0.0, 0.0);
        for v in x {
            let s = v + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let power = s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2;
        2.0 * power.max(0.0).sqrt() / x.len() as f64
    }

    /// The real thing: under a hostile global `pause`, an idle chime is
    /// written to the output (its first note, 659.25 Hz, is there), mpv goes
    /// back to idle, and not one event of that load reaches the channel —
    /// then a real load passes through again.
    ///
    /// `ao=pcm` is untimed (the chime ends in tens of milliseconds here); on
    /// real hardware it lasts its duration. Raw s16 mono at 48 kHz without a
    /// header, as the spike did: appending to a WAV rewrites its header.
    #[tokio::test]
    async fn an_aside_on_a_real_mpv_is_heard_and_reaches_no_one() {
        use crate::player::{ChimeRoute, Player};
        if mpv_missing() {
            eprintln!("mpv missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let out = d.join("out.pcm");
        let mpv = d.join("mpv");
        wrapper(
            &mpv,
            &format!(
                "#!/bin/sh\nexec mpv --no-config --ao=pcm --ao-pcm-file='{}' --ao-pcm-append=yes --ao-pcm-waveheader=no \
                 --audio-format=s16 --audio-channels=mono --audio-samplerate=48000 \"$@\"\n",
                out.display()
            ),
        );
        let (tx, mut rx) = mpsc::channel(1024);
        let (player, _child) =
            start_test_mpv(&mpv, d, "", tx).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}
        player.ipc.command(&[json!("set_property"), json!("pause"), json!(true)]).await.unwrap();

        let uri = crate::lock_sound::idle_uri(player.chime_dir(), crate::lock_sound::LockSound::A, crate::lock_sound::Motif::Lock).unwrap();
        player.play_aside(&uri, None).await.unwrap();
        // 0.74 s of s16 mono at 48 kHz, and mpv back to idle.
        let expected_bytes = 2 * 35_520;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let written = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            let idle = player.ipc.command(&[json!("get_property"), json!("idle-active")]).await.unwrap();
            if written >= expected_bytes && idle == json!(true) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "chime not played back to idle: {written} bytes, idle {idle}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let leaked: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(leaked, vec![], "an event of the aside reached the core");

        let raw = std::fs::read(&out).unwrap();
        let x: Vec<f64> = raw.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f64 / 32768.0).collect();
        let onset = x.iter().position(|v| v.abs() > 0.01).expect("the output is silent");
        let first_note = tone_amplitude(&x[onset..(onset + 4800).min(x.len())], 659.25, 48000.0);
        assert!(first_note > 0.05, "no energy at 659.25 Hz: {first_note}");
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Idle);

        // The gate is open again: a real load reaches the core.
        player.play("av://lavfi:sine=frequency=440:duration=1").await.unwrap();
        let mut got = Vec::new();
        let waited = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(ev) = rx.recv().await {
                got.push(ev);
                if got.contains(&Event::FileStarting) && got.contains(&Event::PlaybackActive) {
                    return;
                }
            }
        })
        .await;
        assert!(waited.is_ok(), "the real load after the aside did not reach the core: {got:?}");
    }

    /// The real thing for a second chime: with a timed output the idle
    /// chime lasts its 0.74 s, and while it sounds mpv answers like a real
    /// file — yet the route is `Idle`.
    #[tokio::test]
    async fn a_second_chime_during_a_real_aside_is_idle() {
        use crate::player::{ChimeRoute, Player};
        if mpv_missing() {
            eprintln!("mpv missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mpv = d.join("mpv");
        wrapper(&mpv, "#!/bin/sh\nexec mpv --no-config --ao=null \"$@\"\n");
        let (tx, _rx) = mpsc::channel(1024);
        let (player, _child) = start_test_mpv(&mpv, d, "", tx).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let uri = crate::lock_sound::idle_uri(player.chime_dir(), crate::lock_sound::LockSound::A, crate::lock_sound::Motif::Lock).unwrap();
        player.play_aside(&uri, None).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let idle = player.ipc.command(&[json!("get_property"), json!("idle-active")]).await.unwrap();
        assert_eq!(idle, json!(false), "the chime is still sounding, or the test proves nothing");
        assert!(player.gate().sounding_aside(), "the gate saw the chime start");
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Idle);
    }

    #[tokio::test]
    async fn the_route_of_a_real_playing_mpv_is_live() {
        use crate::player::{ChimeRoute, Player};
        if mpv_missing() {
            eprintln!("mpv missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mpv = d.join("mpv");
        wrapper(&mpv, "#!/bin/sh\nexec mpv --no-config --ao=null \"$@\"\n");
        let (tx, mut rx) = mpsc::channel(1024);
        let (player, _child) =
            start_test_mpv(&mpv, d, "", tx).await;
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Idle);
        player.play("av://lavfi:sine=frequency=440:sample_rate=44100:duration=30").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(ev) = rx.recv().await {
                if ev == Event::PlaybackRestarted {
                    return;
                }
            }
            panic!("event channel closed before playback-restart");
        })
        .await
        .expect("no playback-restart within 10 s");
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        assert_eq!(player.chime_route(A_LOCK).await.unwrap(), ChimeRoute::Live { sample_rate: 44100, channels: 1 });
        player.toggle_pause().await.unwrap();
        let route = player.chime_route(A_LOCK).await.unwrap();
        // A lavfi source is seekable but its `duration` is how far it was
        // decoded (1.4 s here, measured): the near-end guard reads that as
        // the end and keeps it silent. A paused file: `a_paused_chime_on_a_real_mpv_reaches_no_one`.
        assert_eq!(route, ChimeRoute::Silent);
        player.stop().await.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.chime_route(A_LOCK).await.unwrap() != ChimeRoute::Idle {
            assert!(std::time::Instant::now() < deadline, "mpv never went back to idle");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// The real thing for a paused chime, on a seekable file with a timed
    /// output: the route says where the file stands; during the chime mpv
    /// really plays (`time-pos` moves) while `progress` stays at the pause;
    /// after it mpv is paused at that position again; and not one event
    /// reaches the channel — not even after the user resumes, when mpv sends
    /// the restore seek's `playback-restart`, which the gate consumes. A
    /// later seek of the user's reaches the core again.
    #[tokio::test]
    async fn a_paused_chime_on_a_real_mpv_reaches_no_one() {
        use crate::lock_sound::{mix_filter_over_silence, LockSound, LockVolume, Motif};
        use crate::player::{ChimeRoute, Player};
        if mpv_missing() || std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("mpv or ffmpeg missing: skipping test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let bed = d.join("bed.wav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "aevalsrc=exprs='0.3*sin(2*PI*1000*t)|0.3*sin(2*PI*1000*t)':s=48000:d=20"])
            .arg(&bed)
            .status()
            .unwrap();
        assert!(made.success());
        let mpv = d.join("mpv");
        wrapper(&mpv, "#!/bin/sh\nexec mpv --no-config --ao=null \"$@\"\n");
        let (tx, mut rx) = mpsc::channel(1024);
        let (player, _child) = start_test_mpv(&mpv, d, "", tx).await;
        player.play(bed.to_str().unwrap()).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(ev) = rx.recv().await {
                if ev == Event::PlaybackRestarted {
                    return;
                }
            }
        })
        .await
        .expect("no playback-restart within 10 s");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let collect = seen.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                collect.lock().unwrap().push(ev);
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        player.toggle_pause().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        seen.lock().unwrap().clear();

        let route = player.chime_route(A_LOCK).await.unwrap();
        let ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: Some(t) } = route else {
            panic!("not a paused seekable file: {route:?}");
        };
        let at_pause = player.progress().await.unwrap();
        assert_eq!(at_pause.position_s, Some(t));
        assert!((0.5..3.0).contains(&t), "{t}");

        let chain = mix_filter_over_silence(player.chime_dir(), LockSound::A, Motif::Lock, 48000, 2, LockVolume::Two).unwrap();
        player.sound_while_paused(&chain, Some(t)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let real = number_or_none(player.ipc.command(&[json!("get_property"), json!("time-pos")]).await).unwrap();
        assert!(real > t + 0.2, "mpv did not play the chime: time-pos {real} from {t}");
        assert_eq!(player.progress().await.unwrap(), at_pause, "the core saw the chime move the position");
        tokio::time::sleep(std::time::Duration::from_millis(640)).await;
        player.end_sound_while_paused("", Some(t)).await.unwrap();

        let paused = player.ipc.command(&[json!("get_property"), json!("pause")]).await.unwrap();
        assert_eq!(paused, json!(true));
        let af = player.ipc.command(&[json!("get_property"), json!("af")]).await.unwrap();
        assert_eq!(af, json!([]), "the plain chain is back");
        // Not `time-pos` while paused: right after the restore it read
        // 0.17 s short in about one run in four here, the playback then
        // resuming at the restored point all the same (see
        // `end_paused_chime`). The core reads the pause's.
        assert_eq!(player.progress().await.unwrap(), at_pause);

        // The user resumes: the restore's `playback-restart` comes and is
        // consumed by the gate, and the music plays on from where it was
        // paused (`ao=null` is timed).
        let resumed = std::time::Instant::now();
        player.toggle_pause().await.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while player.gate().restore_pending {
            assert!(std::time::Instant::now() < deadline, "mpv never sent the restore's playback-restart");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let now = player.progress().await.unwrap().position_s.unwrap();
        let expected = t + resumed.elapsed().as_secs_f64();
        assert!((now - expected).abs() < 0.15, "playing at {now}, expected about {expected} (paused at {t})");
        assert_eq!(*seen.lock().unwrap(), Vec::<Event>::new(), "an event of the paused chime reached the core");

        player.seek_relative(2).await.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !seen.lock().unwrap().contains(&Event::PlaybackRestarted) {
            assert!(std::time::Instant::now() < deadline, "the user's seek never reached the core");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}
