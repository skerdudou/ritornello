use anyhow::{Context, Result};
use ritornello_proto::{
    SourcesCatalog, Cover, DisplayFrame, Enrichment, IdentityUpdate, NowPlaying, PlayerState, Preset,
    SourceAction, SourceMessage, SourceReq, SourceRequest,
};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Outcome of a request addressed to a Source: the action the core must
/// apply to the player, and possibly a correction of the identity of what is
/// playing.
pub struct SourceOutcome {
    pub action: SourceAction,
    /// Left as `None`, the core's current identity is kept. A Source that
    /// knows what it has just put into playback must fill it in: without it,
    /// no `metadata` plugin learns about the change, and an in-flight
    /// enrichment for the previous track would stay on screen.
    pub identity: Option<IdentityUpdate>,
    /// The status is an ephemeral message (see `SourceMessage::transient`).
    pub transient: bool,
    /// Numbered key matching what is playing (see `SourceMessage::preset`).
    pub preset: Option<u8>,
    /// See `SourceMessage::preset_count`.
    pub preset_count: Option<u8>,
    /// See `SourceMessage::preset_name`.
    pub preset_name: Option<String>,
    /// See `SourceMessage::status`.
    pub status: Option<String>,
    /// See `SourceMessage::presets`.
    pub presets: Option<Vec<Preset>>,
}

impl SourceOutcome {
    /// Outcome carrying only an action (no status, no identity).
    pub fn new(action: SourceAction) -> Self {
        Self {
            action,
            identity: None,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
            presets: None,
        }
    }

    /// Declares the status as an **ephemeral** message: the core shows it for
    /// a few seconds, then brings back the previous permanent status. Use it
    /// to report an incident without destroying the display of what is
    /// playing.
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }

    /// Declares the numbered remote-control key that matches what is playing:
    /// the preset for a radio, the track for a cd. This is what lets the UI
    /// highlight the active key. The core forgets it on its own when nothing
    /// is playing anymore.
    pub fn preset(mut self, n: u8) -> Self {
        self.preset = Some(n);
        self
    }

    /// Declare how many numbered presets exist after this frame (stations,
    /// tracks). See `SourceMessage::preset_count` for the exact semantics.
    pub fn preset_count(mut self, n: u8) -> Self {
        self.preset_count = Some(n);
        self
    }

    /// Declares the human-readable name of the preset carried by `preset`
    /// (see `SourceMessage::preset_name`). The radio plugin uses it with the
    /// station's configured name.
    pub fn preset_name(mut self, name: impl Into<String>) -> Self {
        self.preset_name = Some(name.into());
        self
    }

    /// Declares the source's own state word (see `SourceMessage::status`).
    pub fn status(mut self, word: impl Into<String>) -> Self {
        self.status = Some(word.into());
        self
    }

    /// Declares the source's named presets (see `SourceMessage::presets`).
    ///
    /// **An empty list normalizes to absence**, and that is deliberate: "this
    /// source has no names" and "this frame says nothing about names" are the
    /// same statement, so only one of the two writings may travel, and it is
    /// absence. A caller cannot get this wrong, which is why nothing here asks
    /// them to check first — the older wording did ask ("call this with a
    /// non-empty list"), `Notification::presets` never did, and a source
    /// following the docs literally would have relayed an empty list from the
    /// spontaneous path. Deriving the property beats documenting it twice.
    pub fn presets(mut self, presets: Vec<Preset>) -> Self {
        self.presets = if presets.is_empty() { None } else { Some(presets) };
        self
    }

    /// Declares the **opaque** identity of what is playing from now on.
    pub fn plays(mut self, identity: serde_json::Value) -> Self {
        self.identity = Some(IdentityUpdate::Playing(identity));
        self
    }

    /// Declares that nothing is playing anymore.
    pub fn plays_nothing(mut self) -> Self {
        self.identity = Some(IdentityUpdate::Nothing);
        self
    }
}

/// Spontaneous notification from a Source: track change, delayed arrival of a
/// TOC, disc insertion.
///
/// Deliberately without an action: the core alone decides what goes into
/// playback. A Source that could trigger a `Play` on its own initiative would
/// make playback unpredictable from the remote control.
#[derive(Default)]
pub struct Notification {
    pub identity: Option<IdentityUpdate>,
    /// See `SourceMessage::transient`.
    pub transient: bool,
    /// See `SourceOutcome::preset`.
    pub preset: Option<u8>,
    /// See `SourceMessage::preset_count`.
    pub preset_count: Option<u8>,
    /// See `SourceMessage::preset_name`.
    pub preset_name: Option<String>,
    /// See `SourceMessage::status`.
    pub status: Option<String>,
    /// See `SourceMessage::presets`.
    pub presets: Option<Vec<Preset>>,
    /// See `SourceMessage::cover`.
    pub cover: Option<ritornello_proto::CoverRef>,
}

impl Notification {
    pub fn new() -> Self {
        Self::default()
    }

    /// See `SourceOutcome::preset`.
    pub fn preset(mut self, n: u8) -> Self {
        self.preset = Some(n);
        self
    }

    /// See `SourceMessage::preset_count`.
    pub fn preset_count(mut self, n: u8) -> Self {
        self.preset_count = Some(n);
        self
    }

    /// See `SourceOutcome::preset_name`.
    pub fn preset_name(mut self, name: impl Into<String>) -> Self {
        self.preset_name = Some(name.into());
        self
    }

    /// Declares the source's own state word (see `SourceMessage::status`).
    pub fn status(mut self, word: impl Into<String>) -> Self {
        self.status = Some(word.into());
        self
    }

    /// See `SourceOutcome::presets`. This is what lets a Source **republish**
    /// its catalog without being asked again — renaming a station from its
    /// admin page propagates that way.
    ///
    /// An empty list becomes absence here, exactly as on `SourceOutcome`:
    /// this constructor had neither guard nor warning, and that was the
    /// hole — a Source following the documentation to the letter relayed an
    /// empty list through the spontaneous path.
    pub fn presets(mut self, presets: Vec<Preset>) -> Self {
        self.presets = if presets.is_empty() { None } else { Some(presets) };
        self
    }

    pub fn plays(mut self, identity: serde_json::Value) -> Self {
        self.identity = Some(IdentityUpdate::Playing(identity));
        self
    }

    pub fn plays_nothing(mut self) -> Self {
        self.identity = Some(IdentityUpdate::Nothing);
        self
    }

    /// See `SourceMessage::cover`.
    pub fn cover(mut self, c: ritornello_proto::CoverRef) -> Self {
        self.cover = Some(c);
        self
    }
}

#[async_trait::async_trait]
pub trait SourcePlugin: Send + 'static {
    async fn activate(&mut self) -> SourceOutcome;
    async fn deactivate(&mut self) -> SourceOutcome;
    async fn select(&mut self, n: u8) -> SourceOutcome;
    async fn next(&mut self) -> SourceOutcome;
    async fn prev(&mut self) -> SourceOutcome;
    async fn eject(&mut self) -> SourceOutcome;

    /// Does this Source have anything to eject?
    ///
    /// A **capability of the Source**, not of what it has loaded: an empty
    /// tray still opens, so the cd answers true without a disc. The sdk
    /// stamps it on every frame, the core relays it in `PlayerState`, and the
    /// web remote greys out its Eject key wherever it leads nowhere — instead
    /// of emitting a command that `eject()` silently drops.
    ///
    /// Default **false**: not knowing means offering nothing. That is what
    /// keeps the capability accurate without touching the plugins that eject
    /// nothing (radio, files, generic input): they compile unchanged and
    /// their key turns grey.
    fn can_eject(&self) -> bool {
        false
    }

    /// Wake-up (boot / leaving standby). By default, behaves like
    /// `activate()` (play) — suited to the radio and to any simple source.
    /// A plugin that must not play on its own at wake-up (cd) overrides it.
    async fn wake(&mut self) -> SourceOutcome {
        self.activate().await
    }

    /// The core stopped playback without consulting the Source (Stop key).
    ///
    /// Default implementation: declare that nothing is playing anymore, which
    /// is true for every Source. Without a status, this frame **erases** the
    /// status memorized core-side (a permanent frame without a status means
    /// erasure, see `SourceMessage::status`) — which is correct here, a
    /// Source with no permanent status having nothing to lose. A Source that
    /// declares one on every frame (the cd) must override and go back through
    /// its own status logic, or watch it vanish on stop; a Source that also
    /// keeps its own playback state (still the cd) overrides too, to bring it
    /// up to date. The others compile unchanged.
    async fn stop(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop).plays_nothing()
    }

    /// The player moved on its own to the track at index `n`.
    ///
    /// Default implementation: nothing — a radio has no tracks. A Source that
    /// tracks an index (the cd) overrides to realign itself and return an
    /// up-to-date identity (and, through its own status, a state).
    async fn player_track(&mut self, _n: i64) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop)
    }

    /// Changes the plugin's current language. Default implementation: no-op —
    /// a plugin with no text of its own (console, mce) has nothing to do, and
    /// cd/radio compile unchanged as long as they have not overridden this
    /// method.
    async fn set_locale(&mut self, _locale: String) {}

    /// The named presets, if this source knows how to enumerate them.
    /// Default: the empty list, which means "I only have numbers". The cd is
    /// in that case by nature — a track has no name without a database — and
    /// the files stay there for now: their list **is** the queue, not a set
    /// of presets.
    ///
    /// The list may be **sparse** (stations 1, 5, 99): `Preset::index` is the
    /// index that `Select` expects, never a rank.
    async fn list_presets(&mut self) -> Vec<Preset> {
        Vec::new()
    }

    /// Spontaneous notification (e.g. track change, delayed arrival of a
    /// TOC). By default never completes: a plugin with no spontaneous
    /// notification (Radio) has nothing extra to write.
    ///
    /// Two contract points, dictated by the harness's `select!`:
    ///
    /// - **`None` is terminal**: it means "no notification ever again" (the
    ///   internal task producing them is dead), and the harness stops calling
    ///   this method — the core's requests keep being served. A `None`
    ///   re-polled in a loop would have spun at 100% CPU with no symptom
    ///   other than the heat.
    /// - **Cancelable without loss**: the future is dropped as soon as a
    ///   request from the core arrives (same requirement, and same reason, as
    ///   `MetadataPlugin::next_enrichment`). Any durable state must live in
    ///   the plugin, not in the future's local variables — two successive
    ///   `await`s whose second one gets interrupted would lose the first.
    async fn poll_notification(&mut self) -> Option<Notification> {
        std::future::pending().await
    }
}

/// Binds a Source's socket, without serving yet.
///
/// Split from `serve_source` so that the `Runtime` can bind **all** its
/// sockets before announcing itself: that ordering is what makes the
/// announcement an availability barrier.
pub fn bind_source(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepts the core's connection, then handles requests and spontaneous
/// notifications until the connection closes.
pub async fn serve_source(listener: UnixListener, mut plugin: impl SourcePlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    // True as long as `poll_notification` has not returned `None` — which is
    // terminal (see the trait) and disarms the matching `select!` arm.
    let mut notifications_open = true;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let req: SourceRequest = match serde_json::from_str(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("invalid source line ignored: {e}");
                        continue;
                    }
                };
                let outcome = match req.req {
                    SourceReq::Activate => plugin.activate().await,
                    SourceReq::Wake => plugin.wake().await,
                    SourceReq::Deactivate => plugin.deactivate().await,
                    SourceReq::Select(n) => plugin.select(n).await,
                    SourceReq::Next => plugin.next().await,
                    SourceReq::Prev => plugin.prev().await,
                    SourceReq::Eject => plugin.eject().await,
                    SourceReq::Stop => plugin.stop().await,
                    SourceReq::PlayerTrack(n) => plugin.player_track(n).await,
                    SourceReq::SetLocale(locale) => {
                        plugin.set_locale(locale).await;
                        SourceOutcome::new(SourceAction::Noop)
                    }
                    // Same precedent as `SetLocale`: a method that does not
                    // return a `SourceOutcome`. The `Noop` is not decorative —
                    // it is what unties the `SourceClient`'s `oneshot`, which
                    // requires `(Some(id), Some(action))`. Without an action,
                    // the caller would wait out the 5 s timeout and then fail,
                    // while the list is already there, right next to it.
                    SourceReq::ListPresets => {
                        // No guard here anymore: `SourceOutcome::presets`
                        // itself normalizes an empty list into absence, for
                        // all its callers and not just this arm (see its doc).
                        // The default body of `list_presets` returns
                        // `Vec::new()`, so a source that does not enumerate
                        // does produce an inert frame — without this path
                        // having to think about it.
                        SourceOutcome::new(SourceAction::Noop)
                            .presets(plugin.list_presets().await)
                    }
                };
                let msg = SourceMessage {
                    id: Some(req.id),
                    action: Some(outcome.action),
                    identity: outcome.identity,
                    transient: outcome.transient,
                    preset: outcome.preset,
                    preset_count: outcome.preset_count,
                    preset_name: outcome.preset_name,
                    status: outcome.status,
                    // Stamped here, once, rather than by a constructor call on
                    // each of a plugin's ten declaration paths: a capability
                    // forgotten on a single path would give a button that
                    // flickers between active and greyed out as frames go by.
                    can_eject: Some(plugin.can_eject()),
                    presets: outcome.presets,
                    // A response to a request (Activate, Select…) never
                    // carries a cover: `SourceOutcome` does not declare it,
                    // only the spontaneous notification does (see below).
                    cover: None,
                };
                write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
            }
            notification = plugin.poll_notification(), if notifications_open => {
                match notification {
                    Some(n) => {
                        let msg = SourceMessage {
                            id: None,
                            action: None,
                            identity: n.identity,
                            transient: n.transient,
                            preset: n.preset,
                            preset_count: n.preset_count,
                            preset_name: n.preset_name,
                            status: n.status,
                            can_eject: Some(plugin.can_eject()),
                            presets: n.presets,
                            cover: n.cover,
                        };
                        write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
                    }
                    // `None` is terminal (see the trait): disarm the arm,
                    // otherwise it would be re-polled immediately and the loop
                    // would spin empty — 100% CPU while requests keep being
                    // served, the quietest failure there is. The case is real:
                    // the cd plugin returns `None` if its player-watching task
                    // dies.
                    None => {
                        tracing::warn!("no more spontaneous notifications (internal task ended)");
                        notifications_open = false;
                    }
                }
            }
        }
    }
}

/// Historical wrapper: binds then serves. Kept for direct calls and for the
/// protocol tests, which must not move.
pub async fn run_source_plugin(plugin: impl SourcePlugin, socket_path: &Path) -> Result<()> {
    serve_source(bind_source(socket_path)?, plugin).await
}

#[async_trait::async_trait]
pub trait DisplayPlugin: Send + 'static {
    async fn show(&mut self, state: PlayerState) -> Result<()>;

    /// The catalog of sources and their named presets.
    ///
    /// Default: **ignored** — a twenty-column display has no use for it, and
    /// this default body is what makes every new frame kind a non-breaking
    /// addition (see `DisplayFrame`, built to grow).
    async fn sources_catalog(&mut self, _c: SourcesCatalog) -> Result<()> {
        Ok(())
    }

    /// Does this display want to receive cover bytes?
    ///
    /// **Default: no.** A cover weighs up to
    /// `ritornello_proto::COVER_MAX_BYTES`, and a twenty-column display has
    /// no use for it: the core must not push megabytes at it that it would
    /// throw away. A display that wants them overrides this method, and it is
    /// **that value** that becomes the announcement flag — see
    /// `Runtime::display`. The announcement is derived, never requested: it
    /// therefore cannot lie about what the plugin will do with the bytes it
    /// receives.
    ///
    /// Read a single time, at registration: the flag goes out on the
    /// registration socket, and the core never reads it again. A display
    /// whose desire would change along the way therefore has nothing to
    /// expect from it — and does not need to: `cover` can simply ignore.
    fn wants_covers(&self) -> bool {
        false
    }

    /// The bytes of the cover of what is playing.
    ///
    /// Default: **ignored** — like `sources_catalog` above, and for the same
    /// reason. Received only if `wants_covers` returns true; the default body
    /// covers a display that would have asked without handling, which the
    /// core has no way to tell apart.
    async fn cover(&mut self, _c: Cover) -> Result<()> {
        Ok(())
    }
}

/// Binds a display's socket, without serving yet.
///
/// Split from `serve_display` so that the `Runtime` can bind **all** its
/// sockets before announcing itself: that ordering is what makes the
/// announcement an availability barrier.
pub fn bind_display(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepts the core's connection, then displays every state received until
/// the connection closes. One-way protocol: no response is expected.
///
/// Each line is a `DisplayFrame`: a full `PlayerState` — not a pre-composed
/// view, layout belongs to the plugin (see
/// `ritornello-plugin-console::display`) —, a catalog of sources, or the
/// bytes of a cover. The latter arrives **only** if the plugin overrode
/// `wants_covers`: it is the core that does not send it, not this SDK that
/// filters it out — a twenty-column display must not receive megabytes on its
/// socket only to drop them on arrival.
///
/// A frame of a kind this SDK does not know is handled like an unreadable
/// line: `warn` then `continue`, the connection survives. That is the policy
/// that makes adding a frame kind non-breaking in both directions — and a
/// line beyond `MAX_LINE` is handled exactly the same way (see
/// `read_line_bounded`), so that the policy stays single.
pub async fn serve_display(listener: UnixListener, mut plugin: impl DisplayPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, _write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = Vec::new();
    loop {
        match read_line_bounded(&mut reader, &mut buffer, MAX_LINE).await? {
            LineRead::Eof => return Ok(()),
            LineRead::TooLong(seen) => {
                tracing::warn!("display frame ignored: line over {MAX_LINE} bytes ({seen} seen)");
                continue;
            }
            LineRead::Line => {}
        }
        let frame: DisplayFrame = match serde_json::from_slice(&buffer) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("invalid display frame ignored: {e}");
                continue;
            }
        };
        match frame {
            DisplayFrame::State(state) => plugin.show(state).await?,
            DisplayFrame::Catalog(c) => plugin.sources_catalog(c).await?,
            DisplayFrame::Cover(c) => plugin.cover(c).await?,
        }
    }
}

/// Cap on one line of this protocol, in bytes.
///
/// **A reopened acceptance, not an oversight.** When this transport was
/// written, reading a line without a bound had been accepted on this
/// reasoning: the core is the only writer on this socket, and bounding the
/// reader would change the unreadable-line policy that the design had frozen.
/// The cover cap was 2 MiB back then. It moved to 20 MiB without this
/// acceptance being re-read, and at that value the two halves of the
/// reasoning no longer hold:
///
/// * The `COVER_MAX_BYTES` cap is checked **at decode time**, that is, after
///   the entire line is resident. Its doc says "the producer never
///   materializes beyond it" — true core-side, false reader-side, which had
///   no bound at all. Not 27 MiB: *whatever the writer chooses to send*. A
///   line without a newline grew the `Vec` all the way to OOM, on a 1 GiB
///   device, in a plugin process that normally weighs a few megabytes. "The
///   core is the only writer" speaks of *trust*; that does not bound a `Vec`,
///   and a derailed core is still the core.
/// * The unreadable-line policy, for its part, does not change: a line that
///   is too long is drained up to its newline then handled like an unreadable
///   line — `warn`, `continue`, the connection survives —, exactly like a
///   malformed frame or one of an unknown kind. That is what makes the
///   refusal consequence-free: a cover frame is **self-contained**, skipping
///   one loses only a picture.
///
/// The value is that of the largest **legitimate** line: 4/3 of
/// `COVER_MAX_BYTES` in base64, plus a margin for the JSON envelope (the
/// keys, the `href`, the MIME type). The `COVER_MAX_BYTES` check at decode
/// time thus remains the only judge of plausibly-sized lines — an image just
/// above the cap is refused by it, with its message, as before. This bound
/// only sees the outrageous.
const MAX_LINE: usize = ritornello_proto::COVER_MAX_BYTES / 3 * 4 + 4 + 4096;

/// Outcome of one bounded line read.
enum LineRead {
    /// A complete line is in the buffer.
    Line,
    /// The line exceeded `MAX_LINE`: nothing is in the buffer, and the rest
    /// of the line has been **consumed** up to its newline — otherwise the
    /// next loop turn would read its middle as if it were a frame. Carries
    /// the number of bytes seen, so the log can state the magnitude.
    TooLong(usize),
    /// End of stream: the peer closed.
    Eof,
}

/// Reads one line into `buffer`, never accumulating more than `cap` bytes in
/// it.
///
/// Written by hand rather than with `BufReader::lines()` or `read_until`:
/// both accumulate without a bound. `fill_buf`/`consume` makes it possible to
/// copy what is useful and to **discard as it flows** what goes over, so the
/// resident peak is `cap` plus the `BufReader`'s internal buffer, whatever
/// length the writer sends.
///
/// The newline is not copied, just as `lines()` did not copy it. A final line
/// without a trailing newline is still returned (`Line`), then the close is
/// seen on the next turn: same behavior as `lines()`.
///
/// `cap` is a **parameter** rather than `MAX_LINE` read directly, so that
/// tests can exercise draining and resynchronization on a few dozen bytes.
/// Building them at the real value would cost 28 MiB per test, and the only
/// effect of that expense would be to load the machine — the logic under test
/// is the same at 16 bytes as at 28 MiB, and it is the logic that can break,
/// not the constant.
async fn read_line_bounded<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<LineRead> {
    use tokio::io::AsyncBufReadExt as _;
    buffer.clear();
    let mut seen = 0usize;
    let mut too_long = false;
    loop {
        // The available content is copied **then** consumed in the same turn:
        // the borrow on `reader` must end before the call to `consume`, hence
        // the block.
        let (done, consumed) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                // End of stream. An unterminated line already started is
                // returned, a line that was too long stays refused.
                if too_long {
                    return Ok(LineRead::TooLong(seen));
                }
                return Ok(if seen == 0 { LineRead::Eof } else { LineRead::Line });
            }
            match available.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if !too_long {
                        buffer.extend_from_slice(&available[..i]);
                        // Checked on this branch too. The `BufReader`'s
                        // internal buffer (8 KiB) cannot hand over in one go a
                        // line longer than `cap`, but making the bound depend
                        // on that size would make it depend on an
                        // implementation detail.
                        if buffer.len() > cap {
                            too_long = true;
                            buffer.clear();
                            buffer.shrink_to_fit();
                        }
                    }
                    seen += i;
                    (true, i + 1)
                }
                None => {
                    seen += available.len();
                    if !too_long {
                        buffer.extend_from_slice(available);
                        if buffer.len() > cap {
                            // Irreversible switch for this line: the buffer is
                            // given back right away rather than kept until the
                            // newline, and the rest is read only to be thrown
                            // away.
                            too_long = true;
                            buffer.clear();
                            buffer.shrink_to_fit();
                        }
                    }
                    (false, available.len())
                }
            }
        };
        reader.consume(consumed);
        if done {
            return Ok(if too_long { LineRead::TooLong(seen) } else { LineRead::Line });
        }
    }
}

/// Historical wrapper: binds then serves. Kept for direct calls and for the
/// protocol tests, which must not move.
pub async fn run_display_plugin(plugin: impl DisplayPlugin, socket_path: &Path) -> Result<()> {
    serve_display(bind_display(socket_path)?, plugin).await
}

use ritornello_proto::InputMessage;

#[async_trait::async_trait]
pub trait InputPlugin: Send + 'static {
    async fn next_command(&mut self) -> Result<InputMessage>;
}

/// Binds an input's socket, without serving yet.
///
/// Split from `serve_input` so that the `Runtime` can bind **all** its
/// sockets before announcing itself: that ordering is what makes the
/// announcement an availability barrier.
pub fn bind_input(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepts the core's connection, then relays every `InputMessage` the plugin
/// produces. `held: false` is not serialized (see `InputMessage`), so the
/// bytes on the wire stay unchanged for non-held commands — a core from
/// before Task 1 would deserialize the frame without seeing anything new in
/// it.
pub async fn serve_input(listener: UnixListener, mut plugin: impl InputPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (_read, mut write) = stream.into_split();
    loop {
        let msg = plugin.next_command().await?;
        write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
    }
}

/// Historical wrapper: binds then serves. Kept for direct calls and for the
/// protocol tests, which must not move.
pub async fn run_input_plugin(plugin: impl InputPlugin, socket_path: &Path) -> Result<()> {
    serve_input(bind_input(socket_path)?, plugin).await
}

#[async_trait::async_trait]
pub trait MetadataPlugin: Send + 'static {
    /// What is playing has changed. The plugin alone decides whether it can
    /// do something with this identity; if it does not recognize it, it stays
    /// silent.
    async fn now_playing(&mut self, np: NowPlaying);

    /// Next available enrichment. Never completes if there is nothing to say
    /// (same convention as `poll_notification`).
    ///
    /// **Must be cancelable without loss**: this future is dropped as soon as
    /// a `NowPlaying` arrives, so any durable state (open HTTP connection,
    /// queue, cache) must live in the plugin, never in the future's local
    /// variables.
    async fn next_enrichment(&mut self) -> Enrichment;
}

/// Binds a metadata plugin's socket, without serving yet.
///
/// Split from `serve_metadata` so that the `Runtime` can bind **all** its
/// sockets before announcing itself: that ordering is what makes the
/// announcement an availability barrier.
pub fn bind_metadata(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepts the core's connection, then relays in both directions until the
/// connection closes: every line received is a `NowPlaying`, every produced
/// enrichment goes out on the wire. No correlation by `id`: the two
/// directions are independent.
pub async fn serve_metadata(listener: UnixListener, mut plugin: impl MetadataPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                match serde_json::from_str::<NowPlaying>(&line) {
                    Ok(np) => plugin.now_playing(np).await,
                    Err(e) => tracing::warn!("invalid metadata line ignored: {e}"),
                }
            }
            enrichment = plugin.next_enrichment() => {
                let line = format!("{}\n", serde_json::to_string(&enrichment)?);
                write.write_all(line.as_bytes()).await?;
            }
        }
    }
}

/// Historical wrapper: binds then serves. Kept for direct calls and for the
/// protocol tests, which must not move.
pub async fn run_metadata_plugin(plugin: impl MetadataPlugin, socket_path: &Path) -> Result<()> {
    serve_metadata(bind_metadata(socket_path)?, plugin).await
}

use ritornello_proto::{AdminReq, AdminRequest, AdminResponse, AdminResult};
use std::collections::HashMap;

#[async_trait::async_trait]
pub trait AdminPlugin: Send + Sync + 'static {
    /// UI asset: `(mime, body)`, or `None` if the path is unknown.
    /// Typically `ui.js` and `ui.css`, embedded via `include_str!`.
    fn asset(&self, path: &str) -> Option<(String, String)>;
    /// The plugin's i18n catalog, flattened.
    ///
    /// `lang = None`: the language the plugin was started in. `Some(l)`:
    /// rebuild for `l` — cheap, `Catalog::load` only parses a TOML pack.
    fn catalog(&self, lang: Option<&str>) -> serde_json::Value;
    async fn get_data(&self) -> serde_json::Value;
    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>;
}

/// Binds an admin plugin's socket, without serving yet.
///
/// Split from `serve_admin` so that the `Runtime` can bind **all** its
/// sockets before announcing itself: that ordering is what makes the
/// announcement an availability barrier.
pub fn bind_admin(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepts the core's connection, then handles admin requests **in
/// parallel**: one task per request, a single writer on the socket.
///
/// Historically serial (read, wait, write, read again), which meant that a
/// `set_data` mounting a sleepy network share held back `ui.js`, a mere
/// `include_str!`, until the core's cap — the admin page "disappeared".
/// Responses now leave in the order they complete; the `id` correlates them,
/// not the order.
///
/// The plugin sits behind an `RwLock`: `asset`, `catalog`, `get_data` read in
/// parallel, `set_data` is exclusive — legitimately so, it is a write. The
/// budget (`deadline_ms`) covers the **wait for the lock** as well as the
/// processing: a `GetCatalog` stuck behind a 60 s `set_data` answers
/// `Expired` at its deadline instead of staying silent.
///
/// `Ping` takes no lock: that is what lets the core tell "busy" from "dead".
/// **Assets** take none either once seen: a bundle is immutable for the
/// process's lifetime, so it is cached here, and the two conventional names
/// (`ui.js`, `ui.css`) are loaded before the first request. Without that, the
/// `RwLock` being fair (FIFO), a `GetAsset` arriving after a queued
/// `set_data` would wait behind it — exactly the incident this decoupling
/// wants to close.
///
/// What the budget **does not absorb**: `tokio::time::timeout` abandons the
/// future at the next `await` point, so an interrupted `set_data` releases
/// the lock — but a blocking IO inside a `spawn_blocking` runs to completion.
/// Plugins that touch a network path therefore keep the obligation to run
/// off-thread and under a circuit breaker (see `plugin-files/src/health.rs`).
pub async fn serve_admin(listener: UnixListener, plugin: impl AdminPlugin) -> Result<()> {
    // The conventional assets are read **before** accepting: the lock is
    // necessarily free, and the core will ask for them from the first page.
    let assets: std::sync::Arc<std::sync::Mutex<HashMap<String, (String, String)>>> = Default::default();
    for name in ["ui.js", "ui.css"] {
        if let Some(a) = plugin.asset(name) {
            assets.lock().unwrap().insert(name.to_string(), a);
        }
    }
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let plugin = std::sync::Arc::new(tokio::sync::RwLock::new(plugin));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AdminResponse>(64);

    // The single writer: serializes outgoing frames without serializing the
    // processing.
    let writer = tokio::spawn(async move {
        while let Some(resp) = rx.recv().await {
            let line = match serde_json::to_string(&resp) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("admin response not serializable: {e}");
                    continue;
                }
            };
            if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: AdminRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("invalid admin request ignored: {e}");
                continue;
            }
        };
        let plugin = plugin.clone();
        let assets = assets.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let id = req.id;
            let budget = req.deadline_ms.map(std::time::Duration::from_millis);
            let work = handle_admin(plugin, assets, req.req);
            let result = match budget {
                Some(d) => match tokio::time::timeout(d, work).await {
                    Ok(r) => r,
                    Err(_) => {
                        tracing::warn!("admin request {id} exceeded its {} ms budget", d.as_millis());
                        AdminResult::Expired
                    }
                },
                None => work.await,
            };
            // The recipient may have left (core disconnected): nothing to do.
            let _ = tx.send(AdminResponse { id, result }).await;
        });
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

/// Is `s` a shape `GetCatalog`'s language may ever be let through as?
///
/// Accepted: non-empty, at most 16 characters, `[A-Za-z0-9_-]` only.
/// Rejected — in particular a path-traversal payload such as
/// `../../../../etc/passwd` — because every `AdminPlugin::catalog`
/// implementation hands its `lang` straight to `Catalog::load`, which builds
/// a filesystem path out of it.
///
/// **Deliberately the same rule as `valid_locale`**
/// (`crates/ritornello-core/src/status/locales.rs`), which is the actual
/// authority — it already gates `PUT /api/locale` and already accepts
/// languages like `pt-BR` and `zh_Hant` that a stricter, hand-rolled "plain
/// locale" grammar (this function's previous shape) would have refused. The
/// two cannot share code — this crate does not depend on the core — but this
/// guard must never be *stricter* than the core's: a value the core accepted
/// and forwarded, then silently downgraded to `None` here, is the same class
/// of bug as forwarding an unvalidated one, one layer down. If `valid_locale`
/// ever changes, mirror the change here too.
///
/// This is only the path-safety net, not a check that the language is one
/// the core actually installed — the core owns that stricter check (against
/// `list_locales`) at its HTTP boundary, because only it knows what is
/// installed.
pub fn is_plain_locale(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn handle_admin<P: AdminPlugin>(
    plugin: std::sync::Arc<tokio::sync::RwLock<P>>,
    assets: std::sync::Arc<std::sync::Mutex<HashMap<String, (String, String)>>>,
    req: AdminReq,
) -> AdminResult {
    match req {
        AdminReq::Ping => AdminResult::Pong,
        AdminReq::GetAsset(path) => {
            let cached = assets.lock().unwrap().get(&path).cloned();
            let found = match cached {
                Some(a) => Some(a),
                None => {
                    let loaded = plugin.read().await.asset(&path);
                    if let Some(a) = &loaded {
                        assets.lock().unwrap().insert(path.clone(), a.clone());
                    }
                    loaded
                }
            };
            match found {
                Some((mime, body)) => AdminResult::Asset { mime, body: Some(body) },
                None => AdminResult::Asset { mime: "text/plain".to_string(), body: None },
            }
        }
        AdminReq::GetCatalog(lang) => {
            // Sanitized **here**, before any plugin ever sees it: every one of
            // them hands `lang` straight to `Catalog::load`, which builds a
            // filesystem path from it (`root/component/{lang}.toml`). This is
            // a trust boundary, not defensive habit — `lang` arrives over IPC
            // (eventually from an HTTP query parameter) and a payload like
            // `../../../../etc/passwd` would otherwise escape the locales
            // root and get served to the browser as catalog entries. A single
            // choke point here, rather than five plugins each having to
            // remember to validate.
            let lang = lang.filter(|l| is_plain_locale(l));
            AdminResult::Catalog(plugin.read().await.catalog(lang.as_deref()))
        }
        AdminReq::GetData => AdminResult::Data(plugin.read().await.get_data().await),
        AdminReq::SetData(data) => match plugin.write().await.set_data(data).await {
            Ok(()) => AdminResult::Set { ok: true, error: None },
            Err(msg) => AdminResult::Set { ok: false, error: Some(msg) },
        },
    }
}

/// Historical wrapper: binds then serves. Kept for direct calls and for the
/// protocol tests, which must not move.
pub async fn run_admin_plugin(plugin: impl AdminPlugin, socket_path: &Path) -> Result<()> {
    serve_admin(bind_admin(socket_path)?, plugin).await
}

#[cfg(test)]
mod admin_server_tests {
    use super::*;
    use ritornello_proto::{AdminResponse, AdminResult};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    struct FakeAdmin {
        data: serde_json::Value,
        /// Duration of a `set_data`: simulates the network mount that never
        /// completes.
        set_delay: std::time::Duration,
    }

    #[async_trait::async_trait]
    impl AdminPlugin for FakeAdmin {
        fn asset(&self, path: &str) -> Option<(String, String)> {
            match path {
                "ui.js" => Some(("text/javascript".into(), "export const contract = 1".into())),
                _ => None,
            }
        }
        fn catalog(&self, _lang: Option<&str>) -> serde_json::Value {
            serde_json::json!({ "btn_save": "Enregistrer" })
        }
        async fn get_data(&self) -> serde_json::Value {
            self.data.clone()
        }
        async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
            tokio::time::sleep(self.set_delay).await;
            if data.get("bad").is_some() {
                return Err("refused".into());
            }
            self.data = data;
            Ok(())
        }
    }

    fn slow_fake(secs: u64) -> FakeAdmin {
        FakeAdmin { data: serde_json::json!({}), set_delay: std::time::Duration::from_secs(secs) }
    }

    async fn connected_client(
        plugin: FakeAdmin,
    ) -> (BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        // The socket must outlive the test: the directory is leaked.
        std::mem::forget(dir);
        let listener = bind_admin(&socket).unwrap();
        tokio::spawn(async move { serve_admin(listener, plugin).await.unwrap() });
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, w) = stream.into_split();
        (BufReader::new(r), w)
    }

    async fn line(r: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> AdminResponse {
        let mut s = String::new();
        r.read_line(&mut s).await.unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[tokio::test]
    async fn a_slow_set_data_does_not_hold_back_ui_js() {
        // The silent-share incident: the admin loop was serial, so a single
        // system call that never completed held back `ui.js`, a mere
        // `include_str!`. Here `set_data` sleeps 3 s; the asset must come back
        // well before that, and **before** the set's response.
        let (mut r, mut w) = connected_client(slow_fake(3)).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"req\":\"GetAsset\",\"arg\":\"ui.js\"}\n").await.unwrap();
        let start = std::time::Instant::now();
        let first = line(&mut r).await;
        assert_eq!(first.id, 2, "the asset must answer before the slow set");
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "{:?}", start.elapsed());
        let second = line(&mut r).await;
        assert_eq!(second.id, 1);
        assert_eq!(second.result, AdminResult::Set { ok: true, error: None });
    }

    #[tokio::test]
    async fn the_budget_is_enforced_by_the_server() {
        // The core grants 200 ms; the set takes 3 s: the plugin says so itself
        // (`Expired`) instead of leaving the client guessing.
        let (mut r, mut w) = connected_client(slow_fake(3)).await;
        w.write_all(b"{\"id\":1,\"deadline_ms\":200,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        let start = std::time::Instant::now();
        let resp = line(&mut r).await;
        assert_eq!(resp.result, AdminResult::Expired);
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
    }

    #[tokio::test]
    async fn ping_answers_pong_even_during_a_set_data() {
        let (mut r, mut w) = connected_client(slow_fake(3)).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"deadline_ms\":500,\"req\":\"Ping\"}\n").await.unwrap();
        let resp = line(&mut r).await;
        assert_eq!((resp.id, resp.result), (2, AdminResult::Pong));
    }

    #[tokio::test]
    async fn get_catalog_waits_for_the_lock_within_its_budget_then_expires() {
        // The catalog reads the plugin's state, so it waits for an ongoing
        // `set_data` to finish; if the budget is shorter than that set, it is
        // `Expired`, not a silence.
        let (mut r, mut w) = connected_client(slow_fake(3)).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"deadline_ms\":300,\"req\":\"GetCatalog\",\"arg\":null}\n").await.unwrap();
        let resp = line(&mut r).await;
        assert_eq!((resp.id, resp.result), (2, AdminResult::Expired));
    }

    #[tokio::test]
    async fn getasset_getdata_setdata_getcatalog_dialogue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let socket_srv = socket.clone();
        tokio::spawn(async move {
            run_admin_plugin(
                FakeAdmin { data: serde_json::json!({"n": 1}), set_delay: std::time::Duration::ZERO },
                &socket_srv,
            )
                .await
                .unwrap();
        });

        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = stream.expect("admin connection").into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"id\":1,\"req\":\"GetAsset\",\"arg\":\"ui.js\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Asset { body: Some(ref b), .. } if b.contains("contract")));

        write.write_all(b"{\"id\":2,\"req\":\"GetData\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Data(ref v) if v["n"] == 1));

        write.write_all(b"{\"id\":3,\"req\":\"SetData\",\"arg\":{\"bad\":true}}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Set { ok: false, .. }));

        write.write_all(b"{\"id\":4,\"req\":\"GetAsset\",\"arg\":\"unknown.txt\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Asset { body: None, .. }));

        write.write_all(b"{\"id\":5,\"req\":\"GetCatalog\",\"arg\":null}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Catalog(ref v) if v["btn_save"] == "Enregistrer"));
    }

    /// A test-only admin plugin whose catalog simply echoes the requested
    /// language, so the wiring from the wire to `AdminPlugin::catalog` can be
    /// observed without a real i18n pack.
    struct LangEchoAdmin;

    #[async_trait::async_trait]
    impl AdminPlugin for LangEchoAdmin {
        fn asset(&self, _path: &str) -> Option<(String, String)> {
            None
        }
        fn catalog(&self, lang: Option<&str>) -> serde_json::Value {
            serde_json::json!({ "lang": lang })
        }
        async fn get_data(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn set_data(&mut self, _data: serde_json::Value) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_catalog_request_carries_the_language_through_to_the_plugin() {
        // The language must be **obeyed**, not merely used as a cache key: the
        // URL that asks for `fr` is served `immutable` (Task 8), so it must
        // contain French whatever the plugin's current locale is. Otherwise
        // the promise lies.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        std::mem::forget(dir);
        let listener = bind_admin(&socket).unwrap();
        tokio::spawn(async move { serve_admin(listener, LangEchoAdmin).await.unwrap() });
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(b"{\"id\":1,\"req\":\"GetCatalog\",\"arg\":\"fr\"}\n").await.unwrap();
        let resp = line(&mut r).await;
        let AdminResult::Catalog(v) = resp.result else { panic!("expected a Catalog result: {resp:?}") };
        assert_eq!(v["lang"], "fr");
    }

    #[tokio::test]
    async fn a_malformed_language_never_reaches_the_plugin_and_falls_back_to_none() {
        // Trust boundary: `lang` arrives over IPC (and, once the core wires
        // its HTTP query parameter through, ultimately from a browser), and a
        // plugin hands it straight to `Catalog::load`, which builds a
        // filesystem path from it (`root/component/{lang}.toml`). A
        // path-traversal payload must never reach that call: it must produce
        // exactly the same answer as `None`, not an error and not the
        // requested file's contents.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        std::mem::forget(dir);
        let listener = bind_admin(&socket).unwrap();
        tokio::spawn(async move { serve_admin(listener, LangEchoAdmin).await.unwrap() });
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);

        w.write_all(b"{\"id\":1,\"req\":\"GetCatalog\",\"arg\":\"../../../../etc/passwd\"}\n")
            .await
            .unwrap();
        let malicious = line(&mut r).await;

        w.write_all(b"{\"id\":2,\"req\":\"GetCatalog\",\"arg\":null}\n").await.unwrap();
        let none = line(&mut r).await;

        assert_eq!(malicious.result, none.result, "a malformed language must answer exactly like None");
        let AdminResult::Catalog(v) = malicious.result else { panic!("expected a Catalog result") };
        assert_eq!(v["lang"], serde_json::Value::Null, "the plugin must never see the raw payload");
    }

    #[test]
    fn is_plain_locale_accepts_only_the_installable_shape() {
        // Same bounds as `valid_locale`'s own test
        // (`status::locales::tests::valid_locale_accepts_codes_and_refuses_the_rest`):
        // this guard must accept everything that one does, or a value the
        // core installed and forwarded would be silently downgraded here.
        for ok in ["en", "fr", "pt-BR", "zh_Hant", "fr-CA"] {
            assert!(is_plain_locale(ok), "{ok} should be accepted");
        }
        for bad in ["", "..", "../fr", "fr/..", "../../../../etc/passwd", "fr toml", &"a".repeat(17)] {
            assert!(!is_plain_locale(bad), "{bad} should be rejected");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::SourceAction;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[test]
    fn an_empty_list_becomes_absence_on_both_constructors() {
        // **Both**, in the same test, because their divergence was the defect:
        // `SourceOutcome::presets` merely asked "call me with a non-empty
        // list" and `Notification::presets` said nothing at all. A Source
        // following the documentation to the letter therefore relayed an
        // empty list through the spontaneous path — a frame that declares
        // nothing. The property is now derived on both sides, and that is
        // what this test pins down.
        assert_eq!(
            SourceOutcome::new(SourceAction::Noop).presets(Vec::new()).presets,
            None,
            "SourceOutcome must normalize the empty list into absence"
        );
        assert_eq!(
            Notification::new().presets(Vec::new()).presets,
            None,
            "Notification must normalize it the same way"
        );
    }

    #[test]
    fn a_non_empty_list_travels_unchanged_on_both_constructors() {
        // The counterpart of the test above: the normalization must not
        // swallow what a source actually declares.
        let list = vec![Preset { index: 5, name: "FIP".into() }];
        assert_eq!(
            SourceOutcome::new(SourceAction::Noop).presets(list.clone()).presets,
            Some(list.clone())
        );
        assert_eq!(Notification::new().presets(list.clone()).presets, Some(list));
    }

    #[test]
    fn the_builder_count_lands_in_the_frame() {
        let o = SourceOutcome::new(SourceAction::Noop).preset_count(23);
        assert_eq!(o.preset_count, Some(23));
        let n = Notification::new().preset_count(0);
        assert_eq!(n.preset_count, Some(0));
    }

    #[test]
    fn the_builder_name_lands_in_the_frame() {
        let o = SourceOutcome::new(SourceAction::Noop).preset(4).preset_name("FIP");
        assert_eq!(o.preset, Some(4));
        assert_eq!(o.preset_name.as_deref(), Some("FIP"));
    }

    #[test]
    fn the_notification_carries_a_cover_via_its_constructor() {
        let n = Notification::new()
            .cover(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/cover.jpg".into() });
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/cover.jpg".into() })
        );
        // The other fields do not move: that is the trap of a builder.
        assert_eq!(n.preset, None);
        assert_eq!(n.status, None);
        assert!(!n.transient);
    }

    #[test]
    fn the_builder_status_lands_in_the_frame() {
        let o = SourceOutcome::new(SourceAction::Noop).status("PAS DE DISQUE");
        assert_eq!(o.status.as_deref(), Some("PAS DE DISQUE"));
        let n = Notification::new().status("FIP").preset_name("FIP");
        assert_eq!(n.status.as_deref(), Some("FIP"));
        assert_eq!(n.preset_name.as_deref(), Some("FIP"));
    }

    struct EchoSource;

    #[async_trait::async_trait]
    impl SourcePlugin for EchoSource {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::play("http://fip"))
                .plays(serde_json::json!({"kind": "stream", "url": "http://fip"}))
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Stop).plays_nothing()
        }
        async fn select(&mut self, n: u8) -> SourceOutcome {
            SourceOutcome::new(SourceAction::play(format!("http://station-{n}")))
        }
        async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
    }

    #[tokio::test]
    async fn request_response_dialogue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        // give the server time to bind the socket
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("plugin connection");
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.action, Some(SourceAction::play("http://fip")));
        assert_eq!(
            msg.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream", "url": "http://fip"})))
        );
        // The eject-capability stamp, **derived** from the plugin and not
        // declared by it: `EchoSource` does not override `can_eject`, so the
        // value must be `Some(false)` — present, and false. It is `Some(_)`
        // that carries the property (see the test dedicated to the two frame
        // paths); `false` additionally proves it is not hard-wired.
        assert_eq!(
            msg.can_eject,
            Some(false),
            "the correlated response must carry the capability read from the plugin: {line}"
        );

        write.write_all(b"{\"id\":2,\"req\":\"Select\",\"arg\":3}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(2));
        assert_eq!(msg.action, Some(SourceAction::play("http://station-3")));
    }

    #[tokio::test]
    async fn can_eject_is_stamped_on_both_frame_paths() {
        // **The load-bearing line nothing pinned down.** `serve_source` writes
        // two kinds of frames — the response correlated to a request, and the
        // spontaneous notification — and stamps `can_eject: Some(…)` on each.
        // It is one of the two mechanisms holding shut a class of defect that
        // appeared **three times** in this project: a relayed frame that
        // declares neither identity nor status *erases* the source's
        // memorized status core-side, and the stamp is what guarantees the
        // interesting-frame predicate always sees something. One forgotten
        // path, and "PAS DE DISQUE" would disappear from the screen.
        //
        // **Both** paths in a single test, because the double stamp is the
        // property: proving it on one path would leave the other free to
        // regress.
        //
        // The notification carries only a **cover**, without identity or
        // status: that is the actual shape of a production spontaneous
        // notification, the one that precisely needs the stamp to be relayed.
        struct EjectableSource {
            announced: bool,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for EjectableSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            fn can_eject(&self) -> bool {
                true
            }
            async fn poll_notification(&mut self) -> Option<Notification> {
                if self.announced {
                    // A single notification, then never again: `pending` and
                    // not `None`, which would be terminal and disarm the arm.
                    std::future::pending().await
                } else {
                    self.announced = true;
                    Some(Notification::new().cover(ritornello_proto::CoverRef::Path {
                        path: "/mnt/nas/A/folder.jpg".into(),
                    }))
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EjectableSource { announced: false }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();

        // The two frames arrive in an order the `select!` arm order does not
        // guarantee: we read both and sort them on `id`, rather than assuming
        // which one comes first. No time margin — both must arrive, so both
        // are awaited.
        let mut correlated = None;
        let mut spontaneous = None;
        for _ in 0..2 {
            let line = lines.next_line().await.unwrap().expect("the plugin must write two frames");
            let msg: SourceMessage = serde_json::from_str(&line).unwrap();
            if msg.id.is_some() {
                correlated = Some((msg, line));
            } else {
                spontaneous = Some((msg, line));
            }
        }

        let (correlated, corr_line) = correlated.expect("the response correlated to Activate");
        assert_eq!(correlated.id, Some(1));
        assert_eq!(
            correlated.can_eject,
            Some(true),
            "path 1: the correlated response must stamp the capability: {corr_line}"
        );

        let (spontaneous, spont_line) = spontaneous.expect("the spontaneous notification");
        assert_eq!(
            spontaneous.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/folder.jpg".into() }),
            "the notification under test must be the one that carries only a cover: {spont_line}"
        );
        assert!(
            spontaneous.identity.is_none() && spontaneous.status.is_none(),
            "otherwise the frame would qualify by itself and the stamp would no longer be \
             load-bearing: {spont_line}"
        );
        assert_eq!(
            spontaneous.can_eject,
            Some(true),
            "path 2: the spontaneous notification must stamp the capability too: {spont_line}"
        );
    }

    /// Source whose notification stream dries up: first call `None`, then
    /// counts the re-polls — there must not be any.
    struct DriedUpSource {
        polls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait::async_trait]
    impl SourcePlugin for DriedUpSource {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Noop)
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Noop)
        }
        async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn poll_notification(&mut self) -> Option<Notification> {
            let n = self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                None
            } else {
                std::future::pending().await
            }
        }
    }

    #[tokio::test]
    async fn a_none_from_poll_notification_is_terminal_and_not_re_polled() {
        // Regression (review 2026-07-27): `None` was ignored and the arm
        // re-polled immediately — a hot loop at 100% CPU while requests kept
        // being served. The case is real: the cd plugin returns `None` if its
        // player-watching task dies.
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        let server_polls = polls.clone();
        tokio::spawn(async move {
            run_source_plugin(DriedUpSource { polls: server_polls }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        // Requests keep being served after the stream dries up…
        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        // …and the `None` was read only once: no re-poll. The pause gives the
        // loop time to consume the `None` (the order of a `select!`'s arms is
        // random) — with the old code, the counter would be at 2 here, the
        // arm having been re-polled right away.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wake_defaults_to_activate() {
        // EchoSource does NOT override wake(): it must behave like activate().
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Wake\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.action, Some(SourceAction::play("http://fip")));
    }

    #[tokio::test]
    async fn overridden_wake_is_dispatched() {
        struct WakingSource;
        #[async_trait::async_trait]
        impl SourcePlugin for WakingSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::play("http://activate")) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn wake(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::play("http://wake")) }
        }
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(WakingSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Wake\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        // wake() dispatched (http://wake), NOT activate() (http://activate).
        assert_eq!(msg.action, Some(SourceAction::play("http://wake")));
    }

    #[tokio::test]
    async fn set_locale_is_forwarded_to_the_plugin_and_answers_noop() {
        use std::sync::{Arc, Mutex};
        struct RecordingLocale {
            seen: Arc<Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for RecordingLocale {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn set_locale(&mut self, locale: String) {
                *self.seen.lock().unwrap() = Some(locale);
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        let seen = Arc::new(Mutex::new(None));
        let seen_srv = seen.clone();
        tokio::spawn(async move {
            run_source_plugin(RecordingLocale { seen: seen_srv }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"SetLocale\",\"arg\":\"fr\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.action, Some(SourceAction::Noop));
        assert_eq!(seen.lock().unwrap().as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn list_presets_answers_a_correlatable_noop_with_the_list_alongside() {
        // The two halves of the property, in a single frame: the `Noop`
        // (without it, the `SourceClient`'s `oneshot`, which requires
        // `(Some(id), Some(action))`, would wait out the 5 s timeout) and the
        // list, which travels alongside and not inside the action.
        struct NamingSource;
        #[async_trait::async_trait]
        impl SourcePlugin for NamingSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn list_presets(&mut self) -> Vec<Preset> {
                vec![
                    Preset { index: 1, name: "FIP".into() },
                    Preset { index: 5, name: "France Info".into() },
                ]
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(NamingSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"ListPresets\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(
            msg.action,
            Some(SourceAction::Noop),
            "without an action, the correlation never unties and the caller waits 5 s: {line}"
        );
        assert_eq!(
            msg.presets.as_deref(),
            Some(
                &[
                    Preset { index: 1, name: "FIP".into() },
                    Preset { index: 5, name: "France Info".into() },
                ][..]
            ),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_source_that_does_not_enumerate_declares_no_list() {
        // `EchoSource` does NOT override `list_presets`: the default body
        // returns `Vec::new()`, and the arm must silence it — "no names" and
        // "said nothing" being the same statement, only one of the two
        // writings travels.
        //
        // This is not cosmetic: a `"presets":[]` on the wire would pass the
        // `SourceClient`'s interesting-frame predicate, and a relayed frame
        // that declares neither identity nor status **erases** the core's
        // memorized status. Every source that names nothing would thus wipe
        // its "PAS DE DISQUE" at the first enumeration. The end-to-end proof
        // is client-side:
        // `a_source_that_does_not_enumerate_does_not_wake_the_core`.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"ListPresets\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: SourceMessage = serde_json::from_str(&line).unwrap();
        // The correlation still unties: the `Noop` is there.
        assert_eq!(msg.action, Some(SourceAction::Noop));
        assert_eq!(msg.presets, None, "{line}");
        assert!(!line.contains("presets"), "nothing of the list must travel: {line}");
    }

    #[tokio::test]
    async fn a_spontaneous_notification_can_republish_presets() {
        // The renaming path: the radio re-saves its configuration and pushes
        // its catalog again without being asked. The frame is spontaneous (no
        // `id`) and carries no action.
        struct RenamingSource {
            emitted: bool,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for RenamingSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn poll_notification(&mut self) -> Option<Notification> {
                if self.emitted {
                    std::future::pending::<()>().await;
                }
                self.emitted = true;
                Some(
                    Notification::new()
                        .presets(vec![Preset { index: 2, name: "Nova".into() }]),
                )
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(RenamingSource { emitted: false }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, _write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, None, "a spontaneous notification is not correlated: {line}");
        assert_eq!(
            msg.presets.as_deref(),
            Some(&[Preset { index: 2, name: "Nova".into() }][..]),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_spontaneous_notification_carries_the_identity() {
        // This is the path of a disc's track change and of a TOC's delayed
        // arrival: no request from the core, yet the identity changes.
        struct SpontaneousSource {
            emitted: bool,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for SpontaneousSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn poll_notification(&mut self) -> Option<Notification> {
                if self.emitted {
                    std::future::pending::<()>().await;
                }
                self.emitted = true;
                Some(Notification::new().plays(serde_json::json!({"kind": "disc", "track": 2})))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(SpontaneousSource { emitted: false }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, _write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, None, "a notification is correlated to no request");
        assert_eq!(msg.action, None, "a notification never triggers an action");
        assert_eq!(
            msg.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({"kind": "disc", "track": 2})))
        );
    }

    #[tokio::test]
    async fn source_ignores_invalid_line_and_answers_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("plugin connection").into_split();
        let mut lines = BufReader::new(read).lines();
        // Malformed line: must be ignored (warn + continue), without closing the connection.
        write.write_all(b"this is not json\n").await.unwrap();
        // Valid request afterwards: a normal response is expected.
        write.write_all(b"{\"id\":7,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(7));
        assert_eq!(msg.action, Some(SourceAction::play("http://fip")));
    }

    struct InMemory {
        received: std::sync::Arc<std::sync::Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for InMemory {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            self.received.lock().unwrap().push(state);
            Ok(())
        }
    }

    #[tokio::test]
    async fn bind_then_serve_behaves_like_run() {
        // The split must change nothing observable: a socket bound by
        // `bind_display` accepts a connection BEFORE `serve_display` runs
        // (that is the kernel backlog, and it is what makes the Runtime's
        // announcement reliable).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let listener = bind_display(&socket).unwrap();

        // Nobody serves yet: the connection must nonetheless succeed.
        let stream = UnixStream::connect(&socket).await.expect("the backlog accepts before accept()");

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_plugin = received.clone();
        tokio::spawn(async move {
            serve_display(listener, InMemory { received: received_plugin }).await.unwrap();
        });

        let (_r, mut w) = stream.into_split();
        let frame = DisplayFrame::State(PlayerState::default());
        w.write_all(format!("{}\n", serde_json::to_string(&frame).unwrap()).as_bytes())
            .await
            .unwrap();

        for _ in 0..100 {
            if received.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the state did not reach the plugin");
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use ritornello_proto::PlayerState;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct RecordingDisplay {
        states: Arc<Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for RecordingDisplay {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            self.states.lock().unwrap().push(state);
            Ok(())
        }
    }

    #[tokio::test]
    async fn receives_the_player_state_over_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let plugin = RecordingDisplay::default();
        let states = plugin.states.clone();
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            let _ = run_display_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("display plugin connection");
        use tokio::io::AsyncWriteExt;
        let mut write = stream;
        let e = PlayerState { source: "radio".into(), preset: Some(1), preset_name: Some("FIP".into()), ..Default::default() };
        let frame = DisplayFrame::State(e.clone());
        write.write_all(format!("{}\n", serde_json::to_string(&frame).unwrap()).as_bytes()).await.unwrap();

        for _ in 0..50 {
            if !states.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(states.lock().unwrap().as_slice(), &[e]);
    }

    #[tokio::test]
    async fn a_display_ignoring_the_catalog_still_receives_states() {
        // The default body's property: `RecordingDisplay` does not override
        // `sources_catalog` — like `console` and the three other stubs, which
        // were not touched — and a catalog frame must neither break it nor
        // make it lose the next frame.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        let states = plugin.states.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;

        // First the catalog, which this plugin ignores…
        let cat = DisplayFrame::Catalog(SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![Preset { index: 1, name: "FIP".into() }],
            }],
        });
        w.write_all(format!("{}\n", serde_json::to_string(&cat).unwrap()).as_bytes())
            .await
            .unwrap();
        // …then the state, which must arrive.
        let e = PlayerState { source: "radio".into(), preset: Some(1), ..Default::default() };
        let state = DisplayFrame::State(e.clone());
        w.write_all(format!("{}\n", serde_json::to_string(&state).unwrap()).as_bytes())
            .await
            .unwrap();

        for _ in 0..100 {
            if !states.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // A single one received, and it is the state: the catalog was not
        // taken for an empty state, and did not bring the connection down.
        assert_eq!(
            states.lock().unwrap().as_slice(),
            &[e],
            "the state must get through despite the catalog, and the catalog must not pass for a state"
        );
    }

    #[tokio::test]
    async fn an_interested_display_receives_the_catalog() {
        // The counterpart: the default body must not *swallow* the catalog.
        // Without the routing arm in `serve_display`, this plugin would never
        // see anything.
        #[derive(Clone, Default)]
        struct Interested {
            catalogs: Arc<Mutex<Vec<SourcesCatalog>>>,
        }
        #[async_trait::async_trait]
        impl DisplayPlugin for Interested {
            async fn show(&mut self, _state: PlayerState) -> Result<()> {
                Ok(())
            }
            async fn sources_catalog(&mut self, c: SourcesCatalog) -> Result<()> {
                self.catalogs.lock().unwrap().push(c);
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = Interested::default();
        let seen = plugin.catalogs.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        let expected = SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![Preset { index: 99, name: "Nova".into() }],
            }],
        };
        let frame = DisplayFrame::Catalog(expected.clone());
        w.write_all(format!("{}\n", serde_json::to_string(&frame).unwrap()).as_bytes())
            .await
            .unwrap();
        for _ in 0..100 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(seen.lock().unwrap().as_slice(), &[expected]);
    }

    #[tokio::test]
    async fn an_unreadable_frame_does_not_close_the_connection() {
        // The unreadable-line policy does not change with the envelope:
        // `warn` then `continue`. A frame of a kind this SDK does not know
        // falls into the same case — that is what makes adding a kind
        // non-breaking in both directions. A frame of a **known** kind whose
        // payload is malformed (the `cover` missing its fields below) too:
        // it is the same serde error path.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        let states = plugin.states.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        w.write_all(b"this is not json\n").await.unwrap();
        w.write_all(b"{\"frame\":\"cover\",\"data\":{\"url\":\"x\"}}\n").await.unwrap();
        w.write_all(b"{\"frame\":\"nonexistent-kind\",\"data\":{}}\n").await.unwrap();
        let e = PlayerState { source: "cd".into(), ..Default::default() };
        let state = DisplayFrame::State(e.clone());
        w.write_all(format!("{}\n", serde_json::to_string(&state).unwrap()).as_bytes())
            .await
            .unwrap();
        for _ in 0..100 {
            if !states.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(states.lock().unwrap().as_slice(), &[e]);
    }

    // -- the cover frame ----------------------------------------------------

    /// A display that overrode nothing: neither `wants_covers` nor `cover`.
    /// That is the console, and the three other stubs in this file.
    #[tokio::test]
    async fn a_display_ignoring_covers_still_receives_states() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        assert!(!plugin.wants_covers(), "the default body must refuse the bytes");
        let states = plugin.states.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        // A valid cover, which the default body must swallow silently…
        let cover = DisplayFrame::Cover(Cover {
            href: "/api/cover/1a2b".into(),
            mime: "image/jpeg".into(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0],
        });
        w.write_all(format!("{}\n", serde_json::to_string(&cover).unwrap()).as_bytes())
            .await
            .unwrap();
        // …then the state, which must arrive: the connection survived.
        let e = PlayerState { source: "cd".into(), ..Default::default() };
        w.write_all(
            format!("{}\n", serde_json::to_string(&DisplayFrame::State(e.clone())).unwrap())
                .as_bytes(),
        )
        .await
        .unwrap();
        for _ in 0..100 {
            if !states.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(states.lock().unwrap().as_slice(), &[e]);
    }

    #[tokio::test]
    async fn an_interested_display_receives_the_cover_bytes() {
        #[derive(Clone, Default)]
        struct Interested {
            covers: Arc<Mutex<Vec<Cover>>>,
        }
        #[async_trait::async_trait]
        impl DisplayPlugin for Interested {
            async fn show(&mut self, _state: PlayerState) -> Result<()> {
                Ok(())
            }
            fn wants_covers(&self) -> bool {
                true
            }
            async fn cover(&mut self, c: Cover) -> Result<()> {
                self.covers.lock().unwrap().push(c);
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = Interested::default();
        assert!(plugin.wants_covers());
        let seen = plugin.covers.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        // Bytes that are not text, `0x0A` included: this is what the wire
        // encoding must keep intact, and the newline is precisely the
        // protocol's separator.
        let mut bytes = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        bytes.extend((0u16..=255).map(|b| b as u8));
        let expected = Cover {
            href: "/api/cover/1a2b3c4d".into(),
            mime: "image/jpeg".into(),
            bytes,
        };
        w.write_all(
            format!("{}\n", serde_json::to_string(&DisplayFrame::Cover(expected.clone())).unwrap())
                .as_bytes(),
        )
        .await
        .unwrap();
        for _ in 0..100 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(seen.lock().unwrap().as_slice(), &[expected]);
    }

    #[tokio::test]
    async fn a_cover_beyond_the_cap_is_an_unreadable_line_and_the_connection_survives() {
        // The transport cap seen from the receiving side: a refusal, handled
        // by the unreadable-line policy — `warn` then `continue` — and not an
        // allocation of the announced size. The state frame that follows
        // proves the connection survived.
        //
        // The line is built by hand: the producer, for its part, cannot emit
        // this (it never materializes beyond the cap), so only a line written
        // here puts the refusal on the path.
        //
        // Since the reader was bounded, this test also holds the half "the
        // reader's bound does not preempt the decode-time one": this line
        // exceeds `COVER_MAX_BYTES` but stays **under** `MAX_LINE`, so it
        // crosses the reader and it is indeed the deserializer that refuses
        // it — the refusal policy the brief expects is intact.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        let states = plugin.states.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        let oversized = "A".repeat(ritornello_proto::COVER_MAX_BYTES / 3 * 4 + 8);
        w.write_all(
            format!(
                r#"{{"frame":"cover","data":{{"href":"/api/cover/x","mime":"image/jpeg","bytes":"{oversized}"}}}}{}"#,
                "\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let e = PlayerState { source: "cd".into(), ..Default::default() };
        w.write_all(
            format!("{}\n", serde_json::to_string(&DisplayFrame::State(e.clone())).unwrap())
                .as_bytes(),
        )
        .await
        .unwrap();
        for _ in 0..100 {
            if !states.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(states.lock().unwrap().as_slice(), &[e]);
    }

    #[tokio::test]
    async fn a_line_beyond_the_cap_is_drained_without_desynchronizing_the_stream() {
        // **The reader's own bound**, distinct from the cover cap. That one
        // is checked at decode time, so *after* the entire line is resident;
        // `lines()` had no bound at all — a line without a newline grew the
        // buffer as far as the writer cared to go, on a 1 GiB device.
        //
        // What a test can prove here is not residency but what would break if
        // the draining were badly written: the line beyond the cap is
        // **consumed up to its newline**, and the next one is read as a whole
        // line, not as the middle of the previous one. A miscounted `consume`
        // would desynchronize the stream forever.
        //
        // The cap is passed as a parameter: exercising it at the real value
        // would cost 28 MiB per test for exactly the same logic, and that
        // expense would have no effect other than loading the machine.
        let input: &[u8] = b"before\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nafter\n";
        let mut reader = BufReader::new(input);
        let mut buffer = Vec::new();

        assert!(matches!(
            read_line_bounded(&mut reader, &mut buffer, 16).await.unwrap(),
            LineRead::Line
        ));
        assert_eq!(buffer, b"before", "a line under the cap passes intact, without its newline");

        match read_line_bounded(&mut reader, &mut buffer, 16).await.unwrap() {
            LineRead::TooLong(seen) => {
                assert_eq!(seen, 40, "the log must be able to state the magnitude actually seen");
                assert!(buffer.is_empty(), "and nothing of the refused line must stay in memory");
            }
            LineRead::Line => panic!("the 40-byte line should have been refused, not returned"),
            LineRead::Eof => panic!("the stream should not have been exhausted"),
        }

        assert!(matches!(
            read_line_bounded(&mut reader, &mut buffer, 16).await.unwrap(),
            LineRead::Line
        ));
        assert_eq!(
            buffer, b"after",
            "the next line must be read whole: resynchronization is the property \
             this test holds"
        );

        assert!(matches!(
            read_line_bounded(&mut reader, &mut buffer, 16).await.unwrap(),
            LineRead::Eof
        ));
    }

    #[test]
    fn the_line_cap_lets_the_largest_legitimate_cover_through() {
        // The half of the property the test above does not cover: this bound
        // must **never** take the place of the `COVER_MAX_BYTES` refusal,
        // which is the one carrying the message and the policy frozen by the
        // brief. An image of exactly `COVER_MAX_BYTES` is allowed to be
        // emitted, so its line must pass the reader and only be judged at
        // decode time.
        //
        // Verified by arithmetic rather than by building the line: building
        // it would cost 28 MiB to prove an inequality between two constants.
        let base64 = ritornello_proto::COVER_MAX_BYTES.div_ceil(3) * 4;
        assert!(
            MAX_LINE >= base64 + 512,
            "MAX_LINE ({MAX_LINE}) must exceed the base64 of the largest cover \
             ({base64}) by a margin covering the JSON envelope"
        );
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    /// Test plugin: memorizes what it is told and returns an enrichment
    /// echoing the last identity received.
    struct Echoing {
        received: Arc<Mutex<Vec<NowPlaying>>>,
        to_say: Option<Enrichment>,
    }

    #[async_trait::async_trait]
    impl MetadataPlugin for Echoing {
        async fn now_playing(&mut self, np: NowPlaying) {
            self.received.lock().unwrap().push(np.clone());
            self.to_say = np.identity.map(|identity| Enrichment {
                identity,
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                ..Default::default()
            });
        }
        async fn next_enrichment(&mut self) -> Enrichment {
            match self.to_say.take() {
                Some(e) => e,
                // Nothing to say: never completes (the future will be dropped
                // by the runner's `select!` as soon as a NowPlaying arrives).
                None => std::future::pending().await,
            }
        }
    }

    async fn connect(socket: &std::path::Path) -> UnixStream {
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(socket).await {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("could not connect to the metadata plugin");
    }

    #[tokio::test]
    async fn uncorrelated_dialogue_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let socket_srv = socket.clone();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_plugin = received.clone();
        tokio::spawn(async move {
            run_metadata_plugin(Echoing { received: received_plugin, to_say: None }, &socket_srv).await.unwrap();
        });

        let (read, mut write) = connect(&socket).await.into_split();
        let mut lines = BufReader::new(read).lines();

        let np = NowPlaying {
            source: "cd".into(),
            identity: Some(serde_json::json!({"kind": "disc", "track": 0})),
            ..Default::default()
        };
        write.write_all(format!("{}\n", serde_json::to_string(&np).unwrap()).as_bytes()).await.unwrap();

        // The enrichment arrives without being asked for, and without an `id`.
        let line = lines.next_line().await.unwrap().unwrap();
        let e: Enrichment = serde_json::from_str(&line).unwrap();
        assert_eq!(e.identity, serde_json::json!({"kind": "disc", "track": 0}));
        assert_eq!(e.title.as_deref(), Some("So What"));
        assert!(!line.contains("\"id\""), "no correlation by id: {line}");
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_stop_is_forwarded_to_the_plugin() {
        // `identity: null` is the signal that makes the plugin stop its work
        // (close an HTTP connection, forget its cache).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let socket_srv = socket.clone();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_plugin = received.clone();
        tokio::spawn(async move {
            run_metadata_plugin(Echoing { received: received_plugin, to_say: None }, &socket_srv).await.unwrap();
        });

        let mut write = connect(&socket).await;
        write.write_all(b"{\"source\":\"radio\",\"identity\":null}\n").await.unwrap();
        for _ in 0..50 {
            if !received.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].identity, None);
        assert_eq!(received[0].source, "radio");
    }

    #[tokio::test]
    async fn invalid_line_ignored_and_the_next_processed() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let socket_srv = socket.clone();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_plugin = received.clone();
        tokio::spawn(async move {
            run_metadata_plugin(Echoing { received: received_plugin, to_say: None }, &socket_srv).await.unwrap();
        });

        let (read, mut write) = connect(&socket).await.into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"this is not json\n").await.unwrap();
        write.write_all(b"{\"source\":\"cd\",\"identity\":{\"k\":1}}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let e: Enrichment = serde_json::from_str(&line).unwrap();
        assert_eq!(e.identity, serde_json::json!({"k": 1}));
        assert_eq!(received.lock().unwrap().len(), 1, "only the valid frame counts");
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use ritornello_proto::Command;

    struct FixedCommands {
        remaining: Vec<InputMessage>,
    }

    #[async_trait::async_trait]
    impl InputPlugin for FixedCommands {
        async fn next_command(&mut self) -> anyhow::Result<InputMessage> {
            if self.remaining.is_empty() {
                std::future::pending::<()>().await;
            }
            Ok(self.remaining.remove(0))
        }
    }

    #[tokio::test]
    async fn commands_sent_over_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let socket_for_server = socket.clone();
        let plugin = FixedCommands {
            remaining: vec![InputMessage::from(Command::Select(3)), InputMessage::from(Command::Stop)],
        };
        tokio::spawn(async move {
            let _ = run_input_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("input plugin connection");
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();

        let l1 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<InputMessage>(&l1).unwrap(), InputMessage::from(Command::Select(3)));
        let l2 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<InputMessage>(&l2).unwrap(), InputMessage::from(Command::Stop));
    }

    #[tokio::test]
    async fn a_held_message_serializes_held_true_an_unheld_one_omits_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let socket_for_server = socket.clone();
        let plugin = FixedCommands {
            remaining: vec![
                InputMessage::from(Command::VolumeUp),
                InputMessage { cmd: Command::VolumeUp, held: true },
            ],
        };
        tokio::spawn(async move {
            let _ = run_input_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("input plugin connection");
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();

        let l1 = lines.next_line().await.unwrap().unwrap();
        assert!(!l1.contains("held"), "held:false must not appear on the wire: {l1}");
        let l2 = lines.next_line().await.unwrap().unwrap();
        assert!(l2.contains("\"held\":true"), "held:true must appear on the wire: {l2}");
    }
}
