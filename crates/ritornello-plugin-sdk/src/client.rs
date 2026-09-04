use anyhow::{bail, Context, Result};
use ritornello_proto::{
    AdminReq, AdminRequest, AdminResponse, AdminResult, SourcesCatalog, Cover, CoverRef, DisplayFrame,
    Enrichment, IdentityUpdate, InputMessage, NowPlaying, PlayerState, Preset, SourceAction,
    SourceMessage, SourceReq, SourceRequest,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

/// What a Source reports spontaneously or alongside a response: a correction
/// of the identity of what is playing, a status, a preset.
///
/// All these fields travel together because they are produced together by the
/// plugin, in a single frame: splitting them into several channels would
/// create instants where the displayed state and the identity announced to
/// `metadata` plugins contradict each other.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceUpdate {
    pub identity: Option<IdentityUpdate>,
    /// See `SourceMessage::transient`.
    pub transient: bool,
    /// See `SourceMessage::preset`. Absent = nothing declared, keep the
    /// current value.
    pub preset: Option<u8>,
    /// See `SourceMessage::preset_count`.
    pub preset_count: Option<u8>,
    /// See `SourceMessage::preset_name`.
    pub preset_name: Option<String>,
    /// See `SourceMessage::status`.
    pub status: Option<String>,
    /// See `SourceMessage::can_eject`. Absent = nothing declared, keep the
    /// current value. The **only** field that does not arm the relayable-frame
    /// predicate by itself, because it is the only one the SDK stamps on
    /// *every* frame: that predicate is derived from a comparison with a frame
    /// carrying only this field (see below), so its exception does not have to
    /// be written twice. See the field's doc for the why.
    pub can_eject: Option<bool>,
    /// See `SourceMessage::presets`. Arms the relayable-frame predicate by
    /// itself, unlike `can_eject`: it is the only way a list reaches the core,
    /// the response correlated to `ListPresets` being just a `Noop`.
    ///
    /// **Danger, for the core's attention.** A frame carrying only presets
    /// declares **neither identity nor status**, and a permanent frame without
    /// a status means *erasure* of the memorized status
    /// (`Core::handle_source_update`: `if !update.transient { self.source_status
    /// = update.status.clone(); }`). That is the exact reason why `can_eject`
    /// alone leaves a frame inert — waking those frames would erase
    /// "PAS DE DISQUE" from the screen — and why this field breaks the
    /// invariant that made that choice safe ("every path of a real source
    /// declares an identity or a status").
    ///
    /// The core therefore handles presets **and returns before** the status
    /// handling when the frame declares neither identity nor status
    /// (`handle_source_update`, early return): the predicate there restates
    /// the invariant word for word, and covers at the same time the case of
    /// `preset_count`, which was already breaking it in service. Two
    /// mitigations already exist upstream, but neither suffices: the sdk never
    /// emits an empty list (a source that does not enumerate thus stays inert,
    /// see the `ListPresets` arm of `serve_source`), and the catalog is a fact
    /// about a source, not about what is playing — so it is read before the
    /// active-source guard. A source that **does enumerate** (the radio) does
    /// reach this path.
    pub presets: Option<Vec<Preset>>,
    /// See `SourceMessage::cover`. **Absent = nothing declared, keep the
    /// current value** — same convention as `preset`/`preset_count`: a Source
    /// does not repeat the cover on every status frame that follows, so
    /// `Core::set_source_cover` must only be called when this field is `Some`,
    /// never on every relayed frame. Sent alone, as a spontaneous notification
    /// (`id: None`), without identity or status — that is precisely the shape
    /// in which a cover arrives (see the doc of `SourceMessage::cover`, which
    /// explains why it does not wait for the `Play` response). This field
    /// therefore arms the relayable-frame predicate by itself, and it now arms
    /// it **by derivation**, without being named anywhere. It had to be added
    /// by hand to a disjunction once, and until then every Source cover was
    /// silently dropped.
    pub cover: Option<CoverRef>,
}

pub struct SourceClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<SourceAction>>>>,
    next_id: AtomicU64,
}

impl SourceClient {
    pub async fn connect(
        socket_path: &Path,
        name: String,
        update_tx: mpsc::Sender<(String, SourceUpdate)>,
    ) -> Result<Arc<Self>> {
        Self::connect_with_close(socket_path, name, update_tx, None).await
    }

    /// Like `connect`, but notifies `closed_tx` when the socket closes.
    ///
    /// **A variant rather than one more parameter on `connect`**: the close
    /// only interests the core, and nine call sites in this file's tests have
    /// nothing to say about it. A field added to a public signature is paid
    /// for in literals to copy everywhere else.
    ///
    /// A `oneshot` and not an `mpsc`: the close only happens once per client,
    /// and the type says so. What it means exactly is *the peer closed* —
    /// either its process is dead, or it closed its socket. In both cases it
    /// is no longer reachable, which is all the caller needs; it is however
    /// not strict proof of death, and nobody should infer an exit code from
    /// it.
    ///
    /// The SDK knows **nothing** of the core's bookkeeping (name, wiring
    /// generation): it reports a fact, the caller dresses it up. That is what
    /// lets this function stay indifferent to how the core tells two
    /// incarnations of the same plugin apart.
    pub async fn connect_with_close(
        socket_path: &Path,
        name: String,
        update_tx: mpsc::Sender<(String, SourceUpdate)>,
        closed_tx: Option<oneshot::Sender<()>>,
    ) -> Result<Arc<Self>> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to {}", socket_path.display()))?;
        let (read, write) = stream.into_split();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self { writer: Mutex::new(write), pending: pending.clone(), next_id: AtomicU64::new(1) });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let msg = match serde_json::from_str::<SourceMessage>(&line) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("invalid source message ignored: {e}");
                        continue;
                    }
                };
                if let (Some(id), Some(action)) = (msg.id, msg.action.clone())
                    && let Some(tx) = pending.lock().await.remove(&id)
                {
                    let _ = tx.send(action);
                }
                let carries_identity = msg.identity.is_some();
                // **Exhaustive** literal, without `..`: adding a field to
                // `SourceUpdate` no longer compiles until someone has copied
                // it here. This is the "the question is forced" half of the
                // guard-rail.
                let update = SourceUpdate {
                    identity: msg.identity,
                    transient: msg.transient,
                    preset: msg.preset,
                    preset_count: msg.preset_count,
                    preset_name: msg.preset_name,
                    status: msg.status,
                    can_eject: msg.can_eject,
                    presets: msg.presets,
                    cover: msg.cover,
                };
                // And here is the "the answer is forced too" half. The
                // predicate deciding whether this frame is worth relaying is
                // **derived**, never enumerated: a frame is interesting if it
                // carries something beyond what `serve_source` stamps on *all*
                // its frames.
                //
                // There used to be a hand-written disjunction here, and it
                // cost twice: `presets` then `cover` had to be retrofitted
                // into it, and in the meantime a frame carrying only the new
                // field was dropped **silently** — the literal above forced
                // the field to be named, the `if` did not. A condition one
                // must remember to extend always ends up not being extended.
                // `SourceUpdate` derives `PartialEq` and `Default`, so the
                // comparison suffices and an added field enters the predicate
                // without anyone thinking about it.
                //
                // The reference's `..Default::default()` is correct **and**
                // necessary here, unlike in the literal above: a field's
                // default value is precisely "nothing declared", which is what
                // an inert frame must carry.
                //
                // `can_eject` is the only field stamped on every frame (see
                // its doc on the `SourceMessage` side), so the only one taken
                // over from the received frame. A frame that would carry only
                // it thus stays dropped, which was the original choice and
                // must remain so: waking those frames would erase
                // "PAS DE DISQUE" from the screen, a permanent frame without a
                // status meaning erasure core-side.
                //
                // One assumed difference with the old disjunction: a frame
                // carrying only `transient: true`, with no word to display,
                // used to be dropped and now passes. That is more correct —
                // the core disarms an in-flight `+NN` there, and nothing else:
                // the frame recomposes the view, so the memorized status is
                // not touched.
                let inert = SourceUpdate { can_eject: update.can_eject, ..Default::default() };
                if update != inert && update_tx.try_send((name.clone(), update)).is_err() {
                    // A lost status or preset is repaired by the next frame, a
                    // lost **identity** never is — the Source only re-emits it
                    // on change, so the core keeps the previous track's and
                    // the `metadata` plugins keep enriching it, without the
                    // staleness guard seeing anything.
                    //
                    // Always `try_send` and not `send().await`: this same task
                    // delivers the responses correlated to the core's
                    // requests. Waiting here on a full channel would hold back
                    // the response the core is waiting for, and the core only
                    // drains the channel by returning to its loop — a cross
                    // deadlock until `request`'s 5 s timeout. Losing a frame
                    // while flagging it loudly beats a second of frozen
                    // device.
                    if carries_identity {
                        tracing::error!(
                            "identity update for {name} lost (channel full): display and metadata possibly stale until next change"
                        );
                    } else {
                        tracing::warn!("source update for {name} lost (channel full)");
                    }
                }
            }
            // Disconnection: drain the in-flight requests. Dropping each
            // Sender makes request()'s rx.await resolve to Err immediately.
            pending.lock().await.clear();
            // The name, which used to be missing: "source plugin connection
            // closed" without saying which one was unusable on a device that
            // carries several.
            tracing::warn!("source plugin {name} connection closed");
            // And the notice to whoever asked for it. After the drain, so the
            // core cannot observe "disconnected" before the in-flight
            // requests have been released.
            if let Some(tx) = closed_tx {
                let _ = tx.send(());
            }
        });
        Ok(client)
    }

    pub async fn request(&self, req: SourceReq) -> Result<SourceAction> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = SourceRequest { id, req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(action)) => Ok(action),
            Ok(Err(_)) => bail!("source plugin: response dropped"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("source plugin: request timeout")
            }
        }
    }
}

pub struct DisplayClient {
    writer: Mutex<OwnedWriteHalf>,
}

impl DisplayClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to {}", socket_path.display()))?;
        let (_read, write) = stream.into_split();
        Ok(Arc::new(Self { writer: Mutex::new(write) }))
    }

    /// Pushes a state. On the wire, it is a `DisplayFrame::State`: the old
    /// payload unchanged, in an adjacently-tagged envelope.
    pub async fn send(&self, state: &PlayerState) -> Result<()> {
        self.send_frame(&DisplayFrame::State(state.clone())).await
    }

    /// Pushes the sources catalog. Twin of `send`, on its own channel:
    /// widening the state payload would have republished the state on every
    /// catalog change and vice versa, which the core's equality-based
    /// deduplication does not catch — the two values would change together by
    /// construction.
    pub async fn send_catalog(&self, sources_catalog: &SourcesCatalog) -> Result<()> {
        self.send_frame(&DisplayFrame::Catalog(sources_catalog.clone())).await
    }

    /// Pushes the bytes of a cover. Reserved for the displays that asked for
    /// it in their announcement (`Announcement::covers`): the caller filters,
    /// this client does not know who it serves.
    ///
    /// Takes the cover **by value**, unlike its two twins: those clone a
    /// state of a few hundred bytes, this one would carry up to
    /// `COVER_MAX_BYTES`. A clone would double the measured peak for
    /// nothing — the caller has just materialized these bytes and does
    /// nothing else with them.
    pub async fn send_cover(&self, cover: Cover) -> Result<()> {
        self.send_frame(&DisplayFrame::Cover(cover)).await
    }

    /// Writes a cover line **already encoded** by the caller, shared between
    /// all the relays pushing it: unlike `send_cover`, this client neither
    /// serializes nor encodes anything here, it writes the bytes as they are.
    ///
    /// Designed for the core, which builds the line **once per publication**
    /// (`CoverCache::line`) and shares it via `Arc` between the relays of the
    /// subscribed displays — rather than redoing the copy and the encoding
    /// once per relay, which `send_cover` would do if it were called again
    /// with the same image for each of them. The trailing `\n` is part of
    /// `line`: the caller already pushed it, just as `send_frame` does for
    /// its own frames.
    pub async fn send_cover_line(&self, line: &Arc<str>) -> Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(line.as_bytes()).await?;
        Ok(())
    }

    async fn send_frame(&self, frame: &DisplayFrame) -> Result<()> {
        // `push` rather than a `format!("{}\n", …)`: the latter allocated a
        // second string and copied everything again. Inconsequential for a
        // state, measurable for a cover — the resident peak for a 2 MiB image
        // drops from 3.9 × n to 2.6 × n just by removing that copy. The bytes
        // written to the socket are identical.
        let mut line = serde_json::to_string(frame)?;
        line.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

/// Failure of the admin dialogue with a plugin, **typed** so the core can
/// tell them apart.
///
/// A string was not enough: the core flattened everything into "plugin
/// unreachable", so a dead plugin and a plugin answering too slowly received
/// the same message — the first calls for a restart, the second sends you to
/// look at the network.
///
/// The labels stay in **English**: they go into the logs, like every message
/// of this crate. What reaches the screen comes from the core's catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminIpcError {
    /// The request's budget was exceeded — through silence, or because the
    /// plugin answered `Expired` itself. The cap is no longer 5 s for all:
    /// see [`budget`].
    Timeout,
    /// The socket went down, or the request was drained by a disconnection.
    Closed,
}

impl std::fmt::Display for AdminIpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Wordings unchanged: they are already in the logs of devices in
            // service, and changing them would break any search over them.
            Self::Timeout => write!(f, "admin plugin: request timeout"),
            Self::Closed => write!(f, "admin plugin: response dropped"),
        }
    }
}

impl std::error::Error for AdminIpcError {}

/// Budget granted to an admin request, **by nature**: the core is the one
/// that knows an asset is an `include_str!` and that a `SetData` may mount a
/// network share. A single 5 s cap gave both the same timeout.
pub fn budget(req: &AdminReq) -> std::time::Duration {
    use std::time::Duration;
    match req {
        AdminReq::Ping => Duration::from_millis(500),
        // `GetCatalog` now does disk I/O when it carries a language (a
        // `Catalog::load`, i.e. reading and parsing a small TOML pack), but it
        // stays in `GetAsset`'s bucket: both are a couple of local reads of a
        // similar size to what `GetAsset` already returns under this same
        // cap, nothing like the `GetData`/`SetData` requests that may touch a
        // network share.
        AdminReq::GetAsset(_) | AdminReq::GetCatalog(_) => Duration::from_secs(1),
        AdminReq::GetData => Duration::from_secs(5),
        AdminReq::SetData(_) => Duration::from_secs(30),
    }
}

/// Margin granted to the transport beyond the budget: the server answers
/// `Expired` at the deadline, it must be given the time to say so.
const GRACE: std::time::Duration = std::time::Duration::from_millis(500);

pub struct AdminClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>>,
    next_id: AtomicU64,
}

impl AdminClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to {}", socket_path.display()))?;
        let (read, write) = stream.into_split();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self {
            writer: Mutex::new(write),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let resp = match serde_json::from_str::<AdminResponse>(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("invalid admin response ignored: {e}");
                        continue;
                    }
                };
                if let Some(tx) = pending.lock().await.remove(&resp.id) {
                    let _ = tx.send(resp.result);
                }
            }
            // Disconnection: drain the in-flight requests (see SourceClient).
            pending.lock().await.clear();
            tracing::warn!("admin plugin connection closed");
        });
        Ok(client)
    }

    async fn request(&self, req: AdminReq) -> Result<AdminResult> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let budget = budget(&req);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = AdminRequest { id, deadline_ms: Some(budget.as_millis() as u64), req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(budget + GRACE, rx).await {
            // The plugin is alive and says so itself: same verdict as silence.
            Ok(Ok(AdminResult::Expired)) => Err(AdminIpcError::Timeout.into()),
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(AdminIpcError::Closed.into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(AdminIpcError::Timeout.into())
            }
        }
    }

    pub async fn get_asset(&self, path: &str) -> Result<Option<(String, String)>> {
        match self.request(AdminReq::GetAsset(path.to_string())).await? {
            AdminResult::Asset { mime, body } => Ok(body.map(|b| (mime, b))),
            other => anyhow::bail!("unexpected response to GetAsset: {other:?}"),
        }
    }

    /// `lang = None`: the plugin's current language. `Some(l)`: that language
    /// explicitly, rebuilt by the plugin regardless of its current one — this
    /// is what lets the HTTP layer serve the answer `immutable` under a
    /// versioned URL (see `AdminReq::GetCatalog`).
    pub async fn get_catalog(&self, lang: Option<&str>) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetCatalog(lang.map(str::to_string))).await? {
            AdminResult::Catalog(v) => Ok(v),
            other => anyhow::bail!("unexpected response to GetCatalog: {other:?}"),
        }
    }

    pub async fn get_data(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetData).await? {
            AdminResult::Data(v) => Ok(v),
            other => bail!("unexpected admin response for GetData: {other:?}"),
        }
    }

    pub async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        match self.request(AdminReq::SetData(data)).await? {
            AdminResult::Set { ok: true, .. } => Ok(Ok(())),
            AdminResult::Set { ok: false, error } => Ok(Err(error.unwrap_or_default())),
            other => bail!("unexpected admin response for SetData: {other:?}"),
        }
    }

    /// 500 ms probe, without a lock plugin-side: `Err(Timeout)` = busy,
    /// `Err(Closed)` = dead.
    pub async fn ping(&self) -> Result<()> {
        match self.request(AdminReq::Ping).await? {
            AdminResult::Pong => Ok(()),
            other => bail!("unexpected admin response for Ping: {other:?}"),
        }
    }
}

/// Connects to a `metadata` plugin and moves both directions until the
/// connection closes: what is playing goes down to the plugin, its
/// enrichments go up to the core, tagged with its name (the name is what
/// decides between two plugins, per the declaration order in `plugins.toml`).
///
/// The downward direction goes through a `watch` and not an `mpsc`: only the
/// last value matters, the intermediate ones are worthless, and above all
/// **a slow plugin cannot block the core**. If the core waited on the write
/// to this socket from its main loop, a plugin that no longer reads (but
/// whose process is still alive) would fill the socket buffer and freeze the
/// whole device — exactly the reason the views already go through a `watch`
/// rather than a direct call.
///
/// The current state is sent **on connection**: a plugin starting while a
/// track is playing does not have to wait for the next one to get to work.
///
/// Only returns on error; to be spawned in a dedicated task by the caller.
pub async fn run_metadata_client(
    socket_path: &Path,
    name: String,
    enrich_tx: mpsc::Sender<(String, Enrichment)>,
    mut np_rx: tokio::sync::watch::Receiver<NowPlaying>,
) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to {}", socket_path.display()))?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    let mut to_send = Some(np_rx.borrow_and_update().clone());
    loop {
        if let Some(np) = to_send.take() {
            write.write_all(format!("{}\n", serde_json::to_string(&np)?).as_bytes()).await?;
        }
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    bail!("metadata plugin {name} connection closed");
                };
                match serde_json::from_str::<Enrichment>(&line) {
                    // `cleaned` here, as close to the input as possible: the
                    // core then has only one shape to handle (see `is_empty`,
                    // which decides the arbitration).
                    Ok(e) => {
                        if enrich_tx.send((name.clone(), e.cleaned())).await.is_err() {
                            bail!("core closed, stopping metadata relay {name}");
                        }
                    }
                    Err(e) => tracing::warn!("invalid enrichment from {name} ignored: {e}"),
                }
            }
            change = np_rx.changed() => {
                if change.is_err() {
                    bail!("now-playing channel closed, stopping metadata relay {name}");
                }
                to_send = Some(np_rx.borrow_and_update().clone());
            }
        }
    }
}

/// Connects to the input plugin and relays each received `InputMessage` on
/// `cmd_tx`, until the connection closes (only returns on error; to be
/// spawned in a dedicated task by the caller).
///
/// Accepts the full envelope (`{"cmd":...,"held":true}`) as well as the bare
/// pre-Task 1 shape (`{"cmd":...}`): `InputMessage` deserializes both, with
/// `held` falling back to `false` when absent.
pub async fn run_input_client(socket_path: &Path, cmd_tx: mpsc::Sender<InputMessage>) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to {}", socket_path.display()))?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<InputMessage>(&line) {
            Ok(msg) => {
                // Receiver gone = core loop finished: keeping on reading the
                // socket only to throw the commands away would be a task
                // leak. Same handling as the symmetric case in the metadata
                // relay (now-playing channel closed).
                if cmd_tx.send(msg).await.is_err() {
                    bail!("core closed, stopping input relay");
                }
            }
            Err(e) => tracing::warn!("invalid command received from input plugin: {e}"),
        }
    }
    bail!("input plugin connection closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::SourceAction;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn two_displays_coexist_and_receive_the_same_state() {
        // The former singleton (`display_connect = Some(...)`) made the first
        // declared display disappear, without an error. This test verifies
        // the only thing it can verify: two `DisplayClient`s live in parallel
        // on two sockets and each receives the same state.
        //
        // It does NOT prove the absence of interference between them: two
        // lines of JSON do not fill a socket's buffer, so the never-read
        // display would not block either with a single task looping over N
        // clients. Non-interference is guaranteed by construction core-side —
        // one task and one socket per display — not here. Hardening it via a
        // buffer-filling test would be slow and flaky.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.sock");
        let b = dir.path().join("b.sock");
        let la = UnixListener::bind(&a).unwrap();
        let lb = UnixListener::bind(&b).unwrap();

        let client_a = DisplayClient::connect(&a).await.unwrap();
        let client_b = DisplayClient::connect(&b).await.unwrap();

        // `a` is accepted then READ; `b` is accepted and never read.
        let (sa, _) = la.accept().await.unwrap();
        let (_sb, _) = lb.accept().await.unwrap();

        let state = PlayerState::default();
        client_a.send(&state).await.unwrap();
        client_b.send(&state).await.unwrap();
        // A second send to `a` after the one to `b`: both clients each keep
        // their own socket and their own write lock.
        client_a.send(&state).await.unwrap();

        let mut lines = BufReader::new(sa).lines();
        assert!(lines.next_line().await.unwrap().is_some());
        assert!(lines.next_line().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn source_client_correlates_by_id_and_relays_identity_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::play("http://fip")),
                identity: Some(ritornello_proto::IdentityUpdate::Playing(
                    serde_json::json!({"kind": "stream", "url": "http://fip"}),
                )),
                transient: false,
                preset: Some(1),
                preset_count: None,
                preset_name: Some("FIP".into()),
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            let _ = socket_for_server; // keeps the path alive for debugging
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        let action = client.request(ritornello_proto::SourceReq::Activate).await.unwrap();
        assert_eq!(action, SourceAction::play("http://fip"));
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        // The identity and the preset arrive in the same update: that is what
        // guarantees one station is never announced while the other is
        // displayed.
        assert_eq!(
            update.identity,
            Some(ritornello_proto::IdentityUpdate::Playing(
                serde_json::json!({"kind": "stream", "url": "http://fip"})
            ))
        );
        // The preset name travels in the same update as the rest.
        assert_eq!(update.preset, Some(1));
        assert_eq!(update.preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn a_frame_carrying_only_the_count_is_relayed() {
        // A frame carrying only preset_count is "interesting" and must be
        // relayed (same logic as preset).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: Some(5),
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.preset_count, Some(5));
    }

    #[tokio::test]
    async fn a_frame_carrying_only_the_eject_capability_stays_inert_but_travels_with_the_rest() {
        // A **deliberate** decision, and this test is what holds it:
        // `can_eject` does not enter the predicate deciding a frame is worth
        // relaying. The sdk stamps it on every frame; if it made an otherwise
        // empty frame "interesting", a bare response (a radio's `eject()`,
        // say) would reach `handle_source_update` — where a permanent frame
        // without a `status` **erases** the memorized status. "PAS DE DISQUE"
        // would disappear from the screen at the first no-effect command.
        //
        // The capability therefore rides on the frames the core already
        // listens to: every path of a real Source declares an identity or a
        // status.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // First request: response carrying **only** the capability.
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let bare = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: Some(true),
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&bare).unwrap()).as_bytes()).await.unwrap();
            // Second request: the same capability, this time accompanied by a
            // status — that is how it reaches the core for real.
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let dressed = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: Some("AUDIO CD".into()),
                can_eject: Some(true),
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&dressed).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "cd".into(), update_tx).await.unwrap();
        client.request(SourceReq::Eject).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        // The **first** update received is the second frame's: the bare frame
        // produced nothing. Otherwise this `recv` would return an empty
        // status, and the assertion below would fall.
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "cd");
        assert_eq!(update.status.as_deref(), Some("AUDIO CD"), "the bare frame should not have been relayed");
        assert_eq!(update.can_eject, Some(true), "the capability travels with the frame that counts");
    }

    #[tokio::test]
    async fn a_frame_carrying_only_the_name_is_relayed() {
        // This is exactly the trap flagged by the brief: a frame carrying
        // only `preset_name` (with no view, identity, preset or count) must
        // pass the condition deciding a frame is "interesting", or it would
        // be dropped silently.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: Some("FIP".into()),
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn a_frame_carrying_only_the_status_is_relayed() {
        // The same trap as for `preset_name` (see the brief): a frame
        // carrying only `status` (with no view, identity, preset, count or
        // name) must pass the condition deciding a frame is "interesting", or
        // it would be dropped silently.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: Some("PAS DE DISQUE".into()),
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.status.as_deref(), Some("PAS DE DISQUE"));
    }

    #[tokio::test]
    async fn a_list_presets_response_unties_the_correlation_and_relays_the_list() {
        // The two halves in the same round trip, and that is the heart of the
        // design choice: the list cannot travel as the **response** (the
        // `oneshot` carries only a `SourceAction`), so `request` must return
        // without waiting, and the list arrive through the updates channel.
        // The test **sequences** instead of waiting: two responses, the first
        // carrying only `presets`, the second carrying only a status (which
        // the predicate has always relayed). The first update received must
        // be the presets one. Without the `|| msg.presets.is_some()`, this
        // `recv` would return the **status** and the assertion would fall
        // immediately — instead of waiting forever for a frame that will
        // never come.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(req.req, SourceReq::ListPresets);
            let list_msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: Some(vec![Preset { index: 5, name: "FIP".into() }]),
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&list_msg).unwrap()).as_bytes()).await.unwrap();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let status_msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: Some("RADIO".into()),
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&status_msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        // The correlation unties: without the `Noop`, this `await` would last
        // the 5 s timeout and then fail.
        assert_eq!(client.request(SourceReq::ListPresets).await.unwrap(), SourceAction::Noop);
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(
            update.presets.as_deref(),
            Some(&[Preset { index: 5, name: "FIP".into() }][..]),
            "the first update must be the presets one, got {update:?}"
        );
        // The second follows, and it is the status: the order is the wire's.
        let (_, next_update) = update_rx.recv().await.unwrap();
        assert_eq!(next_update.status.as_deref(), Some("RADIO"));
    }

    #[tokio::test]
    async fn a_source_that_does_not_enumerate_does_not_wake_the_core() {
        // The real `serve_source` against the real `SourceClient`, because
        // the defect plays out **between** the two: a source that does not
        // override `list_presets` returns `Vec::new()`, and if that empty
        // list traveled, it would pass the interesting-frame predicate. But a
        // relayed frame without identity or status **erases** the core's
        // memorized status (`Core::handle_source_update`): "PAS DE DISQUE"
        // would disappear from the screen at the first enumeration, on every
        // source that names nothing.
        //
        // The test **sequences** instead of waiting: after the `ListPresets`,
        // an `Activate` whose response carries an identity — thus relayed for
        // sure. The first update received must be that one. With a
        // `Some([])`, it would be the `ListPresets` frame, without identity,
        // and the assertion would fall on the spot instead of waiting in
        // vain.
        struct NoNames;
        #[async_trait::async_trait]
        impl crate::SourcePlugin for NoNames {
            async fn activate(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::play("http://fip"))
                    .plays(serde_json::json!({"kind": "stream"}))
            }
            async fn deactivate(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn select(&mut self, _n: u8) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn next(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn prev(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn eject(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            // `list_presets` is NOT overridden: that is the whole point of
            // the test.
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = crate::bind_source(&socket).unwrap();
        tokio::spawn(async move {
            crate::serve_source(listener, NoNames).await.unwrap();
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "cd".into(), update_tx).await.unwrap();
        // The correlation unties despite the absent list: the `Noop` is there.
        assert_eq!(client.request(SourceReq::ListPresets).await.unwrap(), SourceAction::Noop);
        client.request(SourceReq::Activate).await.unwrap();

        let (name, first) = update_rx.recv().await.unwrap();
        assert_eq!(name, "cd");
        assert!(
            first.identity.is_some(),
            "the first update must be the activate's: the ListPresets response \
             must relay nothing, got {first:?}"
        );
        assert_eq!(first.presets, None);
        // And there was no other: a single frame was worth it.
        assert!(update_rx.try_recv().is_err(), "no other update must be relayed");
    }

    #[tokio::test]
    async fn a_frame_carrying_only_the_cover_is_relayed() {
        // This is exactly the shape in which a cover arrives for real (see
        // the doc of `SourceMessage::cover`, Task 2): a spontaneous
        // notification, later than the `Play` response, with nothing else.
        // Without the entry added to the predicate, it would be dropped
        // silently before even reaching `SourceUpdate`.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }),
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "files".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "files");
        assert_eq!(update.cover, Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }));
    }

    #[tokio::test]
    async fn source_client_relays_nothing_when_the_frame_carries_neither_view_nor_identity() {
        // A SetLocale response, for instance: no point in waking the core's
        // loop for a frame that says nothing about the display.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::SetLocale("fr".into())).await.unwrap();
        assert!(update_rx.try_recv().is_err(), "no update must be relayed");
    }

    #[tokio::test]
    async fn metadata_client_sends_down_the_current_state_then_relays_enrichments_up() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // The plugin receives the current state without asking for
            // anything, then answers echoing the identity received.
            let line = lines.next_line().await.unwrap().unwrap();
            let np: NowPlaying = serde_json::from_str(&line).unwrap();
            let e = Enrichment {
                identity: np.identity.clone().unwrap(),
                // Deliberate spaces: the relay is what normalizes.
                artist: Some("  Mandrillus Sphynx ".into()),
                title: Some("Bikwix".into()),
                ..Default::default()
            };
            write.write_all(format!("{}\n", serde_json::to_string(&e).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (np_tx, np_rx) = tokio::sync::watch::channel(NowPlaying {
            source: "radio".into(),
            identity: Some(serde_json::json!({"kind": "stream", "url": "http://soma"})),
            ..Default::default()
        });
        let (enrich_tx, mut enrich_rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = run_metadata_client(&socket, "ouifm".into(), enrich_tx, np_rx).await;
        });

        let (name, e) = enrich_rx.recv().await.unwrap();
        assert_eq!(name, "ouifm");
        assert_eq!(e.artist.as_deref(), Some("Mandrillus Sphynx"), "whitespace must be trimmed");
        assert_eq!(e.title.as_deref(), Some("Bikwix"));
        assert_eq!(e.identity, serde_json::json!({"kind": "stream", "url": "http://soma"}));
        drop(np_tx);
    }

    #[tokio::test]
    async fn metadata_client_forwards_changes_of_what_is_playing() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<NowPlaying>::new()));
        let seen_srv = seen.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                seen_srv.lock().await.push(serde_json::from_str(&line).unwrap());
            }
        });

        let (np_tx, np_rx) = tokio::sync::watch::channel(NowPlaying {
            source: "radio".into(),
            identity: Some(serde_json::json!({"url": "one"})),
            ..Default::default()
        });
        let (enrich_tx, _enrich_rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = run_metadata_client(&socket, "ouifm".into(), enrich_tx, np_rx).await;
        });

        // Each send is awaited before the next one. This is not test caution:
        // `watch` only guarantees the **last** value, and two consecutive
        // `send`s can legitimately produce only one on the wire. That is the
        // property we want (a slow plugin neither delays the core nor catches
        // up on an uninteresting history), so the test sequences instead of
        // counting frames.
        async fn wait_for(seen: &Arc<Mutex<Vec<NowPlaying>>>, count: usize) -> Vec<NowPlaying> {
            for _ in 0..100 {
                if seen.lock().await.len() >= count {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            seen.lock().await.clone()
        }

        // The current state goes down on connection, without a prior change.
        let received = wait_for(&seen, 1).await;
        assert_eq!(received.first().and_then(|np| np.identity.clone()), Some(serde_json::json!({"url": "one"})));

        np_tx
            .send(NowPlaying {
                source: "radio".into(),
                identity: Some(serde_json::json!({"url": "two"})),
                ..Default::default()
            })
            .unwrap();
        let received = wait_for(&seen, 2).await;
        assert_eq!(received.get(1).and_then(|np| np.identity.clone()), Some(serde_json::json!({"url": "two"})));

        // The stop goes down too: it is the signal that makes the plugin stop
        // its work (cut an open HTTP connection, forget its cache).
        np_tx.send(NowPlaying { source: "radio".into(), identity: None, ..Default::default() }).unwrap();
        let received = wait_for(&seen, 3).await;
        assert_eq!(received.len(), 3, "{received:?}");
        assert_eq!(received[2].identity, None);
    }

    #[tokio::test]
    async fn display_client_sends_the_state_over_the_line() {
        // The content assertions live in the server task: its `JoinHandle`
        // must be **joined**, otherwise a panic there would be swallowed and
        // the test would only prove "send() returns Ok" — it passed with a
        // client writing wrong JSON or the wrong line.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            // The core now writes an envelope: the server must read a
            // `DisplayFrame`, and the variant matters as much as the
            // content — a state pushed as a catalog would go unnoticed by
            // the display.
            match serde_json::from_str::<DisplayFrame>(&line).unwrap() {
                DisplayFrame::State(e) => assert_eq!(e.preset_name.as_deref(), Some("FIP")),
                other => panic!("a state frame was expected, got {other:?}"),
            }
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        let state = PlayerState { source: "radio".into(), preset: Some(1), preset_name: Some("FIP".into()), ..Default::default() };
        client.send(&state).await.unwrap();
        server.await.expect("the server assertions panicked");
    }

    #[tokio::test]
    async fn display_client_sends_the_catalog_on_the_same_socket_after_a_state() {
        // Two frames of different kinds back to back on the **same**
        // connection: that is what the core's relay does when wiring a
        // display. The assertions live in the server task, whose `JoinHandle`
        // is joined — otherwise a panic there would be swallowed and the test
        // would only prove "send_catalog returns Ok".
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected = SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![Preset { index: 5, name: "FIP".into() }],
            }],
        };
        let expected_srv = expected.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let first = lines.next_line().await.unwrap().unwrap();
            match serde_json::from_str::<DisplayFrame>(&first).unwrap() {
                DisplayFrame::State(e) => assert_eq!(e.source, "radio"),
                other => panic!("the first frame must be a state, got {other:?}"),
            }
            let second = lines.next_line().await.unwrap().unwrap();
            match serde_json::from_str::<DisplayFrame>(&second).unwrap() {
                DisplayFrame::Catalog(c) => assert_eq!(c, expected_srv),
                other => panic!("the second frame must be a catalog, got {other:?}"),
            }
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        client.send(&PlayerState { source: "radio".into(), ..Default::default() }).await.unwrap();
        client.send_catalog(&expected).await.unwrap();
        server.await.expect("the server assertions panicked");
    }

    #[tokio::test]
    async fn display_client_writes_a_cover_on_a_single_line() {
        // The property that matters for a newline-delimited protocol: binary
        // bytes — `0x0A` included — must not cut the line. The server reads
        // **one** line and must find the whole image in it; if it took two,
        // the first would be unreadable and the second noise.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut bytes = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        bytes.extend((0u16..=255).map(|b| b as u8));
        let expected = Cover {
            href: "/api/cover/1a2b3c4d".into(),
            mime: "image/jpeg".into(),
            bytes,
        };
        let expected_srv = expected.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            match serde_json::from_str::<DisplayFrame>(&line).unwrap() {
                DisplayFrame::Cover(c) => assert_eq!(c, expected_srv),
                other => panic!("a cover frame was expected, got {other:?}"),
            }
            // Nothing more afterwards: one line for one image, never a split.
            assert!(lines.next_line().await.unwrap().is_none(), "one cover = one line");
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        client.send_cover(expected).await.unwrap();
        drop(client);
        server.await.expect("the server assertions panicked");
    }

    #[tokio::test]
    async fn admin_client_correlates_responses() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // 1st request (get_asset, id=1)
            let _ = lines.next_line().await.unwrap().unwrap();
            write
                .write_all(
                    b"{\"id\":1,\"result\":{\"kind\":\"Asset\",\"data\":{\"mime\":\"text/javascript\",\"body\":\"export default 1\"}}}\n",
                )
                .await
                .unwrap();
            // 2nd request (get_catalog, id=2)
            let _ = lines.next_line().await.unwrap().unwrap();
            write
                .write_all(b"{\"id\":2,\"result\":{\"kind\":\"Catalog\",\"data\":{\"btn_save\":\"Enregistrer\"}}}\n")
                .await
                .unwrap();
            // 3rd request (set_data, id=3)
            let _ = lines.next_line().await.unwrap().unwrap();
            write
                .write_all(b"{\"id\":3,\"result\":{\"kind\":\"Set\",\"data\":{\"ok\":false,\"error\":\"nope\"}}}\n")
                .await
                .unwrap();
            let _ = &write; // keeps the write half alive
            std::future::pending::<()>().await;
        });

        let client = AdminClient::connect(&socket).await.unwrap();
        assert_eq!(
            client.get_asset("ui.js").await.unwrap(),
            Some(("text/javascript".to_string(), "export default 1".to_string()))
        );
        assert_eq!(client.get_catalog(None).await.unwrap(), serde_json::json!({"btn_save": "Enregistrer"}));
        let verdict = client.set_data(serde_json::json!({})).await.unwrap();
        assert_eq!(verdict, Err("nope".to_string()));
    }

    #[test]
    fn the_budget_depends_on_the_request_nature() {
        use std::time::Duration;
        assert_eq!(budget(&AdminReq::Ping), Duration::from_millis(500));
        assert_eq!(budget(&AdminReq::GetAsset("ui.js".into())), Duration::from_secs(1));
        assert_eq!(budget(&AdminReq::GetCatalog(None)), Duration::from_secs(1));
        assert_eq!(budget(&AdminReq::GetData), Duration::from_secs(5));
        assert_eq!(budget(&AdminReq::SetData(serde_json::json!({}))), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn the_deadline_goes_out_in_the_frame_and_expired_becomes_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let l = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::AdminRequest = serde_json::from_str(&l).unwrap();
            assert_eq!(req.deadline_ms, Some(30_000), "SetData carries its budget");
            write.write_all(b"{\"id\":1,\"result\":{\"kind\":\"Expired\"}}\n").await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = AdminClient::connect(&socket).await.unwrap();
        let err = client.set_data(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err.downcast_ref::<AdminIpcError>(), Some(&AdminIpcError::Timeout));
    }

    #[tokio::test]
    async fn an_unanswered_ping_fails_in_under_two_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _guard = stream; // connected, silent
            std::future::pending::<()>().await;
        });
        let client = AdminClient::connect(&socket).await.unwrap();
        let start = std::time::Instant::now();
        let err = client.ping().await.unwrap_err();
        assert_eq!(err.downcast_ref::<AdminIpcError>(), Some(&AdminIpcError::Timeout));
        // 500 ms budget + 500 ms grace: well under the former 5 s.
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
    }

    #[tokio::test]
    async fn input_client_relays_lines_with_and_without_held() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let socket_for_client = socket.clone();
        tokio::spawn(async move {
            let _ = run_input_client(&socket_for_client, tx).await;
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        // A plain line from a pre-envelope plugin, then a held line.
        stream.write_all(b"{\"cmd\":\"VolumeUp\"}\n{\"cmd\":\"VolumeDown\",\"held\":true}\n").await.unwrap();
        let first = rx.recv().await.unwrap();
        assert_eq!(first, ritornello_proto::InputMessage::from(ritornello_proto::Command::VolumeUp));
        let second = rx.recv().await.unwrap();
        assert_eq!(second.cmd, ritornello_proto::Command::VolumeDown);
        assert!(second.held);
    }

    #[tokio::test]
    async fn an_in_flight_request_fails_fast_on_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Reads the request then closes the connection without answering.
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let _ = lines.next_line().await;
            // End of the block: read and _write dropped -> EOF client-side.
        });
        let (update_tx, _update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        let res = client.request(SourceReq::Activate).await;
        let e = res.expect_err("an unanswered request must fail").to_string();
        // The **message** distinguishes the two paths, where a duration
        // measurement only did so by a margin: "response dropped" comes from
        // the pending map drained at EOF, "request timeout" from the 5 s
        // expiry. Asserting the message thus proves exactly what this test
        // means — that the failure is immediate and not awaited — without
        // depending on machine load, which could cross the 2 s vs 5 s margin.
        assert!(
            e.contains("response dropped"),
            "the request must fail through the drained pending map, not the 5 s timeout: {e}"
        );
        assert!(
            !e.contains("timeout"),
            "a failure by expiry would mean the pending map was not drained: {e}"
        );
    }
}
