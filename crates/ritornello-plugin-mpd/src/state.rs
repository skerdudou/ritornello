//! State shared between the `display` half (which receives the core's frames)
//! and the MPD client sessions (which answer playback commands).
//!
//! The delicate point of the whole plugin lives here, and it is not in the
//! protocol: **the missed wakeup**. A client that sends `idle` right after a
//! change must return immediately, not wait for the next change. A `Notify`
//! alone would lose that wakeup — the notification is emitted while the
//! session is still reading its versions and composing its request, hence
//! before it registers, and it would stay silent until the following change.
//! Hence the chosen design: a monotonic counter per subsystem, which the
//! session memorises **at connection time** and carries from command to
//! command, and a **preliminary** comparison in `wait`. That comparison is
//! what forbids the missed wakeup; the `Notify` only serves to avoid polling.
//!
//! The reference is carried by the connection and not re-read at each `idle`,
//! and that is the half of the mechanism that was missing: re-reading it would
//! swallow everything that moved between a client's previous response and its
//! `idle` line — that is, during the only window where it is not listening.
//! See `versions` and `wait`.

use ritornello_proto::{SourcesCatalog, Command, Cover, Playback, PlayerState, Preset, SourceCatalog};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

/// Number of subsystems, hence the size of the counter array. A constant and
/// not a `Subsystem::len()`: it is the bound of the array, it must be known at
/// compile time.
const SUBSYSTEM_COUNT: usize = 4;

/// The subsystems that `idle` knows how to name, in the order in which they
/// index the counter array.
///
/// An `enum #[repr(usize)]` used as an index into a `[u64; 4]`, and not an
/// associative table: the four subsystems are known at compile time, and
/// `versions[subsystem as usize]` cannot fail — no `unwrap` on a `get`, no
/// subsystem one would have forgotten to insert at construction.
///
/// The explicit values are not decorative: they are the index, so **do not
/// reorder** without reordering what the tests compare.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    /// Play, pause, stop, preset change, position.
    Player = 0,
    /// Volume or mute.
    Mixer = 1,
    /// The queue changes. Since the MPD queue *is* the preset list of the
    /// active source, this means: source change.
    Playlist = 2,
    /// The catalog of sources or of their presets changes.
    ///
    /// Its only trigger is `apply_catalog`, and that is the whole point of the
    /// two channels: a state frame, even one changing everything else, never
    /// moves it — otherwise a client subscribed to stored playlists alone
    /// would be woken at every second of playback.
    StoredPlaylist = 3,
}

/// The current cover, as the plugin holds it between two tracks.
///
/// **The bytes are behind an `Arc`, and that is structural**: `Snapshot` is
/// cloned at *every* command of *every* session (see `read`), and a cover
/// weighs up to `ritornello_proto::COVER_MAX_BYTES` — **20 MiB**. A bare
/// `Vec<u8>` would therefore copy twenty mebibytes to answer `ping`. The `Arc`
/// turns the clone into a counter increment.
///
/// **What the `Arc` does not guarantee**, and it has to be written here
/// because this paragraph wrongly promised it: the image exists only once in
/// the process **per generation**. A session answering `albumart` holds its
/// own clone of the `Arc` — in its `Snapshot` *and* in the binary response —
/// for the whole of its `write_all`, so a client that requests a chunk and
/// then stops reading **pins that generation**. A cover pushed meanwhile is
/// one more generation, which another session can pin in turn. The product is
/// thus written plainly: `MAX_SESSIONS × COVER_MAX_BYTES` = 16 × 20 MiB =
/// **320 MiB**, to which is added the generation the state itself holds, i.e.
/// **340 MiB** on a shared one-gibibyte device. See `commands::MAX_CHUNK` for
/// what bounds the rest, and for what is deliberately **not** mitigated.
///
/// `Arc<Vec<u8>>` and not `Arc<[u8]>`: the conversion from the frame's
/// `Vec<u8>` is then a move, where `Arc<[u8]>::from` would reallocate and copy
/// the 20 MiB once more per track.
#[derive(Clone, PartialEq)]
pub struct HeldCover {
    /// Exactly the `cover_href` that the state frame publishes for the same
    /// image. It is **the** correlation between the image and what is playing:
    /// the core sends the state first and the cover next, so there is a window
    /// where the state already names the next track while the held cover is
    /// still the previous one's. Comparing this field to
    /// `state.track.cover_href` is what forbids serving one for the other (see
    /// the `albumart` arm of `commands.rs`).
    pub href: String,
    /// MIME type recognised by the core from the header bytes, never from an
    /// extension. It is the `type:` that `readpicture` publishes.
    pub mime: String,
    pub bytes: Arc<Vec<u8>>,
}

/// Hand-written `Debug`: the derived one would print the twenty mebibytes of
/// the image, and `Snapshot` is `Debug` — so the slightest `assert_eq!` of a
/// failed test would spew the whole image into the output.
impl std::fmt::Debug for HeldCover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldCover")
            .field("href", &self.href)
            .field("mime", &self.mime)
            .field("bytes", &format_args!("{} o", self.bytes.len()))
            .finish()
    }
}

impl From<Cover> for HeldCover {
    fn from(c: Cover) -> Self {
        Self { href: c.href, mime: c.mime, bytes: Arc::new(c.bytes) }
    }
}

/// A cover **as the core pushes one**, for the tests of the three modules that
/// need it (this one, `commands`, `session`).
///
/// The realism of this fixture is not a courtesy: a cover built from a
/// `Default::default()` would prove a causality inside a frame that the
/// producer cannot emit. Three traits are therefore borrowed from the real
/// producer:
///
/// * the `href` has the `/api/cover/{key}` shape that `cover::HREF_PREFIX`
///   builds, and the caller passes it back in `state.track.cover_href` — it is
///   the only correlation that exists between the image and what is playing;
/// * the bytes **start with a real JPEG header**, because the core recognises
///   the MIME from the header bytes and refuses anything it does not
///   recognise: an image whose header were wrong would never be pushed, so a
///   test using one would test the impossible;
/// * the rest is **noise** from a linear congruential generator and not a
///   regular pattern: that is what makes a skipped, duplicated or
///   one-byte-shifted chunk visible, which a constant fill would hide entirely.
#[cfg(test)]
pub(crate) fn test_cover(href: &str, size: usize) -> Cover {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    let mut x: u32 = 0x1234_5678;
    while bytes.len() < size {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((x >> 24) as u8);
    }
    bytes.truncate(size);
    Cover { href: href.to_string(), mime: "image/jpeg".to_string(), bytes }
}

/// Consistent copy of everything a client session needs to read to compose a
/// response: the state pushed by the core, what the plugin believes about
/// playback, and the counters.
///
/// A single snapshot returned at once, and not four accessors: a `status`
/// response publishes the state *and* the queue version, and reading them
/// through two successive lock acquisitions would let them contradict each
/// other in the middle of a response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// The last frame received from the core, **possibly overlaid with an
    /// optimistic layer**: `acknowledge_optimistic` sets there the volume a
    /// session has just requested, before the core has confirmed it (see over
    /// there). Do not read this field as the verbatim of what the core sent —
    /// the next frame restores the truth anyway, and the comparison in
    /// `apply_state` wakes `Mixer` if it contradicts it.
    pub state: PlayerState,
    /// What the plugin **believes** about playback, including a toggle it has
    /// just emitted and that the frame has not confirmed yet: this is the
    /// `pause` race, where a client sending `pause` then `status` in the same
    /// breath would otherwise read the state from before its own command and
    /// show a button that did not move.
    pub optimistic_playback: Playback,
    /// Version counter of the queue, the one `status` publishes under
    /// `playlist`.
    ///
    /// **Monotonic**, never reset to zero: a client compares the version it
    /// holds to this one to know whether it missed something, and a reset to
    /// zero would make it believe it missed nothing while everything changed.
    pub queue_version: u32,
    /// One counter per subsystem, same use but for `idle`: a sleeping session
    /// has memorised this array, compares it to this one, and returns at once
    /// if something moved while it was settling in.
    pub versions: [u64; SUBSYSTEM_COUNT],
    /// The last catalog received from the core: **all** declared sources, in
    /// the cycling order of `SourceCycle`, and the named presets of each one
    /// when it knows how to enumerate them.
    ///
    /// It describes all sources and not only the active one, and that is
    /// indispensable: `listplaylistinfo "radio"` is asked while the cd is
    /// playing. It arrives through a channel distinct from the state frames
    /// (see `apply_catalog`), so it survives any number of frames without
    /// being resent.
    ///
    /// Empty before the first catalog frame: a client will then see an empty
    /// list of stored playlists, and the queue falls back on the synthesis
    /// from `preset_count`. That is indeed the truth of that instant — the
    /// plugin knows nothing of the catalog yet.
    pub sources_catalog: SourcesCatalog,
    /// The last cover received from the core, or `None` as long as none has
    /// arrived — which is the ordinary case of a stream without an image, not
    /// an anomaly.
    ///
    /// **A single one**, never a per-track cache: the device only knows how to
    /// publish the cover of what is playing (see `DisplayPlugin::cover`), and
    /// memorising the previous ones would hold several mebibytes to serve URIs
    /// that nothing plays any more — exactly what the `albumart` arm refuses
    /// to do.
    ///
    /// **And it is released, not merely replaced**: `apply_state` resets it to
    /// `None` as soon as a state frame announces `cover_href: None`. Without
    /// that the plugin kept up to `COVER_MAX_BYTES` **for the life of the
    /// process**, long after playback stopped, for bytes that no command could
    /// serve any more. See the guard on site.
    pub cover: Option<HeldCover>,
}

impl Snapshot {
    /// The named presets of this source, as the catalog gives them — or `None`
    /// if the catalog does not know this name.
    ///
    /// The distinction matters for `listplaylistinfo` and `load`: a name absent
    /// from the catalog is an `ACK 50` ("this playlist does not exist"),
    /// whereas a known name whose list is **empty** is a source that cannot
    /// enumerate — an empty, well-formed response, not an error.
    pub fn source_catalog(&self, name: &str) -> Option<&SourceCatalog> {
        self.sources_catalog.sources.iter().find(|s| s.name == name)
    }

    /// The presets of the **active** source, or an empty slice. The MPD queue
    /// is made of them.
    pub fn active_presets(&self) -> &[Preset] {
        self.source_catalog(&self.state.source).map_or(&[], |s| s.presets.as_slice())
    }
    /// What must be published as playback state: the optimistic one, not the
    /// raw one from the frame.
    ///
    /// Called by the `status` of `commands.rs` since Task 6. Writing it here
    /// rather than at each response site spares each of them from remembering
    /// *which* of the two fields is authoritative — and a test of that module
    /// fails if `status` reads `state.playback`.
    pub fn playback(&self) -> Playback {
        self.optimistic_playback
    }
}

/// What all client sessions share: the current snapshot and the wakeup of the
/// pending `idle`s.
///
/// The lock is a `tokio::sync::RwLock` and not a `Mutex`: the sessions almost
/// only read, and one composing a 51-line `listplaylistinfo` must not delay
/// the others. The only writers are the `display` half (a frame) and a session
/// that has just emitted a command.
#[derive(Default)]
pub struct SharedState {
    inner: RwLock<Snapshot>,
    /// Wakes the pending `idle`s. `notify_waiters` and not `notify_one`: a
    /// change concerns **all** sleepers, and a permit stored for a single one
    /// of them would be worse than useless here — the counter comparison
    /// already plays the role of the memory.
    wakeup: Notify,
}

/// Normal advance of the position clock between two frames, in seconds,
/// beyond which a change is a **seek** and not time passing.
///
/// Five and not one, although the core emits one frame per second: the frames
/// travel through a `watch`, which **coalesces**. A relay momentarily behind —
/// a busy Pi, a cover being read from a share — only receives the last value,
/// and thus sees the clock jump by two, three or four seconds without anyone
/// having seeked. The margin covers that lag. The price is a seek of less than
/// five seconds that does not wake the sleepers; they will read it at their
/// next `status`, where `elapsed` is always right.
const NORMAL_SEEK_S: u32 = 5;

/// Is this position change an event, or merely time passing?
///
/// **This is the fix for the costliest defect of this plugin.** The core
/// pushes a state frame **per second** during playback, and its only moving
/// field is then `position_s`. Comparing it like the others therefore marked
/// `Player` once per second, and `idle player` — on which every MPD client
/// waits — woke at the same pace. M.A.L.P. then re-requested `status`,
/// `currentsong` **and the cover** every second, which explains the observed
/// instability and the vanishing image: an `albumart` restarted endlessly in
/// the middle of its own chunked transfer. Real MPD never emits `player` for
/// the passing of time; `elapsed` is read in `status`, which the client
/// queries when it wants.
///
/// What remains an event:
/// - the appearance or disappearance of the position (a track that starts, a
///   stream without position);
/// - a move backwards, always — it is a requested rewind, or a new track
///   starting from zero;
/// - an advance greater than `NORMAL_SEEK_S`, i.e. a seek and not the clock.
fn position_jump(before: Option<u32>, after: Option<u32>) -> bool {
    match (before, after) {
        (Some(a), Some(b)) => b < a || b - a > NORMAL_SEEK_S,
        // One of the two is absent: presence and absence are two different
        // playback states, and toggling between them is an event.
        (a, b) => a != b,
    }
}

/// Marks a subsystem as having moved, without duplicates.
///
/// The deduplication is not cosmetic: an MPD command list may contain two
/// `pause`, and incrementing the counter twice for a single pass under the
/// lock would publish two changes where there is only one.
fn mark(moved: &mut Vec<Subsystem>, subsystem: Subsystem) {
    if !moved.contains(&subsystem) {
        moved.push(subsystem);
    }
}

/// What an `idle` learned: the subsystems to announce, and the counters of the
/// instant they were observed.
///
/// A struct and not a bare `(Vec, [u64; 4])`: the two fields would get mixed
/// up in use, and the second one carries the subtlety — it is not "the current
/// counters" but "the ones that decided this wakeup".
#[derive(Debug, PartialEq)]
pub struct Wakeup {
    /// The subsystems that moved, in the order the client requested them.
    pub moved: Vec<Subsystem>,
    /// All counters, read under the same lock acquisition as `moved`.
    ///
    /// The caller only keeps the entries of the subsystems it **announces**:
    /// it is the exact equivalent of MPD's "only clear the reported flags".
    pub versions: [u64; SUBSYSTEM_COUNT],
}

impl SharedState {
    /// Copy of the current snapshot. A copy and not a guard: no session must
    /// hold the lock beyond the instant of the read, even if it then composes
    /// a long response.
    ///
    /// Every client session invokes it once per command, to answer from the
    /// copy rather than under the lock. The counters an `idle` memorises are
    /// in that same copy: that is what makes them consistent with the state
    /// published in the same response.
    pub async fn read(&self) -> Snapshot {
        self.inner.read().await.clone()
    }

    /// Copy of the counter array, to be memorised **once per connection** and
    /// carried from command to command until `wait`.
    ///
    /// It is the useful half of the anti-missed-wakeup mechanism, and its
    /// production caller is `session::serve`, **at banner time**: the counters
    /// an `idle` compares are those of the last time this client was
    /// *informed* of a change, never those of the instant it wrote its `idle`
    /// line.
    ///
    /// **What must above all not be redone** — this was the state of this
    /// code, and it was a defect: reading the counters in the `Snapshot` of
    /// the `idle` command itself. That read is indeed consistent with the
    /// state published in the same response, but it **swallows** everything
    /// that moved between the previous response and the `idle` line, i.e.
    /// exactly the window during which an MPD client is not listening. Real
    /// MPD accumulates its flags **per connection** since the connection, and
    /// an event that occurred between two commands is reported to the next
    /// `idle`. For `stored_playlist`, swallowing it is not transient: nothing
    /// will replay it before the next catalog change, so `listplaylists` stays
    /// stale, potentially forever.
    ///
    /// The acceptable direction of error is the other one: a superfluous
    /// wakeup costs the client a redundant query, a missing wakeup costs it
    /// the correctness of its screen (the same trade-off that
    /// `acknowledge_optimistic` and `apply_cover` each state on their side).
    pub async fn versions(&self) -> [u64; SUBSYSTEM_COUNT] {
        self.inner.read().await.versions
    }

    /// Applies a frame from the core: it is authoritative on everything.
    ///
    /// (see also `position_jump`, which decides what the position clock is
    /// worth as an event)
    ///
    /// The moving subsystems are decided **by field-by-field comparison** with
    /// the previous state, and not by the mere fact that a frame arrived: the
    /// core already deduplicates, but a reconnection of the `display` half
    /// resends the current state, and that must not pass for a change —
    /// otherwise every restart of the plugin would wake all clients for
    /// nothing.
    pub async fn apply_state(&self, state: PlayerState) {
        let mut moved = Vec::new();
        {
            let mut inst = self.inner.write().await;
            let before = &inst.state;

            if state.volume != before.volume || state.muted != before.muted {
                mark(&mut moved, Subsystem::Mixer);
            }
            if state.source != before.source {
                // Two subsystems for a single field: the queue *is* the preset
                // list of the active source, so changing source changes the
                // queue (`playlist`); and what is playing changes with it
                // (`player`). A client listening to `player` only must learn
                // that the source changed.
                mark(&mut moved, Subsystem::Playlist);
                mark(&mut moved, Subsystem::Player);
            }
            if state.preset_count != before.preset_count {
                // `preset_count` is what the MPD queue is made of **in the
                // absence of a named list**: for a source that cannot
                // enumerate (the cd, the files), it is all the plugin knows
                // about the queue. An inserted disc goes from `None`/`Some(0)`
                // to `Some(12)` without changing source name, and without this
                // comparison no client would learn there are twelve tracks to
                // play — the most ordinary action there is.
                //
                // `Playlist` alone, and **not** `Player`: it is the queue that
                // changed, not what is playing. (`source` moves both because
                // it changes both; `preset_count` alone does not touch the
                // current track.)
                mark(&mut moved, Subsystem::Playlist);
            }
            if state.playback != before.playback
                || state.preset != before.preset
                || position_jump(before.position_s, state.position_s)
                || state.track != before.track
            {
                mark(&mut moved, Subsystem::Player);
            }

            // The frame overwrites the optimism, including when it contradicts
            // it: the optimism is only a bridge between the emitted command
            // and its confirmation, and letting it outlive a frame would make
            // `status` lie indefinitely had the core refused the toggle.
            inst.optimistic_playback = state.playback;
            inst.state = state;

            // **The cover is released here, and this is the only place that
            // can do it.** `cover_href: None` is the core's signal that nothing
            // playing has an illustration any more; yet `cover` was never
            // reset to `None`, so the plugin kept up to `COVER_MAX_BYTES` —
            // 20 MiB — for the life of the process, including long after
            // playback stopped, on a one-gibibyte device shared with mpv.
            //
            // Those bytes were no longer servable anyway: the `albumart` arm
            // requires the held `href` to be the one the frame announces (see
            // `commands::cover`), so `cover_href: None` had already made them
            // unreachable. Freeing them takes no response away from anyone.
            //
            // **Why this criterion and not "the held `href` differs from the
            // one the frame announces"**, which would free a little earlier:
            // the core sends the state *before* the bytes, so there is a
            // normal window where the frame already announces the next key
            // while the held cover is still the previous one. The strict
            // criterion would destroy there an image that the next frame
            // would have legitimised, should the order of the two channels
            // ever invert. `None` is the only signal that does not depend on
            // that order.
            if inst.state.track.cover_href.is_none() {
                // No wakeup of its own: the frame that turns `cover_href` to
                // `None` changes `track`, so it already marked `Player` above.
                // And in the degenerate case where `track` were identical (a
                // repeated frame after the cover stopped being servable),
                // there is nothing to announce — `albumart` was already
                // refusing.
                inst.cover = None;
            }

            for subsystem in &moved {
                inst.versions[*subsystem as usize] += 1;
            }
            if moved.contains(&Subsystem::Playlist) {
                // Exactly when `Playlist` moves: the two counters say the same
                // thing to two audiences (`idle` and the `playlist` field of
                // `status`), and desynchronising them would make `plchanges`
                // answer beside the wakeup that just left.
                inst.queue_version += 1;
            }
        }
        if !moved.is_empty() {
            tracing::trace!("mpd frame moved subsystems {moved:?}");
            self.wakeup.notify_waiters();
        }
    }

    /// Applies a catalog received from the core: the list of sources and
    /// their named presets.
    ///
    /// **Two subsystems, and not always both.**
    /// - `StoredPlaylist` moves as soon as the catalog differs from the
    ///   previous one: it is the subsystem MPD reserves for stored playlists,
    ///   and each source *is* a stored playlist here.
    /// - `Playlist` (and with it `queue_version`) only moves if the presets of
    ///   the **active** source changed — the queue comes from there and from
    ///   nowhere else. Renaming a station of a source that is not playing
    ///   changes the stored playlists without touching the queue: waking
    ///   `Playlist` would make every client re-download 51 lines for nothing,
    ///   and a `plchanges` would answer an identical queue under a new
    ///   version.
    ///
    /// Comparison and not blind assignment, exactly like `apply_state` and for
    /// the same reason: the core sends the current value **at connection
    /// time**, so a reconnection of the `display` half comes through here
    /// again with an identical catalog, and that must not pass for a change —
    /// otherwise every restart of the plugin would wake all clients.
    pub async fn apply_catalog(&self, sources_catalog: SourcesCatalog) {
        let mut moved = Vec::new();
        {
            let mut inst = self.inner.write().await;
            if inst.sources_catalog == sources_catalog {
                return;
            }
            // Any real change of the catalog moves `StoredPlaylist`: we got
            // past the deduplication above, so the catalog really differs.
            // This is what wakes a client asleep on `idle stored_playlist` —
            // the only subsystem that nothing incremented before this task.
            mark(&mut moved, Subsystem::StoredPlaylist);
            // Read before the overwrite, on the source name of the current
            // snapshot: it is the active source as the last state frame said
            // it, the only authority on what is playing.
            let presets_before = inst.active_presets().to_vec();
            inst.sources_catalog = sources_catalog;
            if inst.active_presets() != presets_before.as_slice() {
                mark(&mut moved, Subsystem::Playlist);
            }

            for subsystem in &moved {
                inst.versions[*subsystem as usize] += 1;
            }
            if moved.contains(&Subsystem::Playlist) {
                // The same pairing as in `apply_state`: the two counters say
                // the same thing to two audiences (`idle` and the `playlist`
                // field of `status`), and desynchronising them would make
                // `plchanges` answer beside the wakeup that just left.
                inst.queue_version += 1;
            }
        }
        if !moved.is_empty() {
            tracing::trace!("mpd sources_catalog moved subsystems {moved:?}");
            self.wakeup.notify_waiters();
        }
    }

    /// Applies a cover received from the core: the bytes that `albumart` and
    /// `readpicture` will serve.
    ///
    /// **The moved subsystem is `Player`, and it is the only available
    /// choice.** The MPD protocol has no subsystem for covers: the list of
    /// names `idle` accepts is fixed by MPD and a `changed: cover` would be
    /// understood by no client. That left choosing among the four this plugin
    /// emits, and `Player` is the one clients really tie to the artwork: an
    /// MPD client re-requests `currentsong` **then** the image on a `player`
    /// wakeup, because the cover is a fact about the current track. `Mixer`
    /// (the volume) and `Playlist` (the queue) trigger no image refresh in the
    /// known clients, and `StoredPlaylist` is reserved for stored playlists.
    ///
    /// This wakeup is not decorative, it is what makes the function useful:
    /// the core sends **the state first, the cover next** (see
    /// `display_relay`). A client woken by the state frame alone therefore
    /// requests its image while the plugin still holds the previous track's —
    /// hence receives a refusal — and without this second wakeup it would
    /// never learn that the image arrived. The price is one more
    /// `changed: player` per track change, the same accepted asymmetry as
    /// `PlayPause` in `acknowledge_optimistic`: a superfluous wakeup costs the
    /// client a redundant query, a missing wakeup costs it an empty cover
    /// until the next track.
    ///
    /// Comparison and not blind assignment, like the two functions above: the
    /// core already pushes only on change, but it also pushes the current
    /// cover **at wiring time**, so a reconnection of the `display` half comes
    /// through here again with the same image and that must wake nobody. The
    /// comparison is on the bytes and not only on the `href`: equality of two
    /// `Arc`s with the same content is settled without a copy, and trusting
    /// the `href` alone would silence a really different image published
    /// under the same key.
    pub async fn apply_cover(&self, cover: Cover) {
        {
            let mut inst = self.inner.write().await;
            let cover = HeldCover::from(cover);
            if inst.cover.as_ref() == Some(&cover) {
                return;
            }
            inst.cover = Some(cover);
            inst.versions[Subsystem::Player as usize] += 1;
        }
        tracing::trace!("mpd cover moved subsystem Player");
        self.wakeup.notify_waiters();
    }

    /// Acknowledges what the plugin has just emitted, before the core confirms
    /// it.
    ///
    /// **Three commands only**, and that is deliberate: `PlayPause` (toggles
    /// `Playing`↔`Paused`), `SetVolume` (sets the volume) and `Mute` (toggles
    /// the mute). Everything else is ignored, because guessing the effect of a
    /// `Select` on the position, the track or the preset would be wrong more
    /// often than right — the active source decides, and it alone. A slightly
    /// late `status` is benign; a `status` that invents a track is not.
    ///
    /// **`Mute` joined the list along with the unmuting `setvol`** (see
    /// `commands::setvol`), and without it that unmute would have been
    /// invisible: `status` publishes `volume: 0` as soon as `state.muted` is
    /// true, so acknowledging `SetVolume(40)` alone would have left a client
    /// reading `volume: 0` right after raising its slider — its slider falling
    /// back to zero, i.e. the exact defect the optimistic layer exists to
    /// avoid. The core, for its part, honours `Mute` unconditionally
    /// (`self.muted = !self.muted`), so the toggle acknowledged here invents
    /// nothing.
    ///
    /// The volume, for its part, is set in `state` for lack of a separate
    /// optimistic field. That is intended and risk-free: the next frame
    /// overwrites it anyway, and had the core clamped or refused the value,
    /// the comparison in `apply_state` will see the difference and wake
    /// `Mixer`. The only side effect is that the *confirming* frame moves
    /// nothing again — hence the increment done right here.
    ///
    /// **The asymmetry with `PlayPause` is intended**, and it has to be
    /// written down because it reads like an oversight: the toggle does not
    /// touch `state.playback`, so the confirming frame moves `Player` a second
    /// time — a redundant `changed: player`. That is the conservative choice.
    /// `SetVolume` carries an absolute value that the core honours almost
    /// always to the bit: without the increment done here, the confirming
    /// frame would be identical and *nobody* would be woken — so there was no
    /// choice. `PlayPause` carries no value: the plugin computes the toggle,
    /// and the active source may well end up elsewhere (a live stream that
    /// cannot be paused). Leaving `state.playback` untouched keeps the frame
    /// as the sole authority on that field, and the price is one wakeup too
    /// many. That price is the right direction of the asymmetry: a
    /// superfluous wakeup costs the client a redundant `status` query, a
    /// missing wakeup costs it the correctness of its screen.
    ///
    /// Called by the session **after** pushing the commands on the channel,
    /// never before: acknowledging a toggle that was not emitted would make
    /// `status` lie until the next frame.
    pub async fn acknowledge_optimistic(&self, commands: &[Command]) {
        let mut moved = Vec::new();
        {
            let mut inst = self.inner.write().await;
            for command in commands {
                match command {
                    Command::PlayPause => match inst.optimistic_playback {
                        // No effect when stopped: `PlayPause` there starts a
                        // playback of which the plugin knows neither what nor
                        // where, so it waits for the frame rather than
                        // announcing `Playing` on an empty track.
                        Playback::Stopped => {}
                        Playback::Playing => {
                            inst.optimistic_playback = Playback::Paused;
                            mark(&mut moved, Subsystem::Player);
                        }
                        Playback::Paused => {
                            inst.optimistic_playback = Playback::Playing;
                            mark(&mut moved, Subsystem::Player);
                        }
                    },
                    Command::SetVolume(level) => {
                        let level = *level;
                        // Comparison and not blind assignment: a `setvol` that
                        // re-sets the current volume (M.A.L.P. sends one at
                        // every slider release) must not wake all the other
                        // clients for nothing.
                        if inst.state.volume != level {
                            inst.state.volume = level;
                            mark(&mut moved, Subsystem::Mixer);
                        }
                    }
                    // Toggle and not assignment, because the command is a
                    // toggle: the core does `muted = !muted` unconditionally
                    // (see `Command::Mute` in the core), so the acknowledgement
                    // here is exact and not guessed. No comparison to make — a
                    // toggle always changes something.
                    Command::Mute => {
                        inst.state.muted = !inst.state.muted;
                        mark(&mut moved, Subsystem::Mixer);
                    }
                    _ => {}
                }
            }
            for subsystem in &moved {
                inst.versions[*subsystem as usize] += 1;
            }
            // No `queue_version` here: none of the acknowledged commands
            // touches the queue.
        }
        if !moved.is_empty() {
            tracing::trace!("mpd optimistic update moved subsystems {moved:?}");
            self.wakeup.notify_waiters();
        }
    }

    /// Waits until one of the requested `subsystems` moves relative to the
    /// `seen` counters, and returns those that moved — in the order they were
    /// requested — **with the counters of the instant that decided**.
    ///
    /// **Compare first, wait next.** If something moved since the caller read
    /// `seen`, the function returns without ever touching the `Notify`: it is
    /// there and nowhere else that the missed wakeup is forbidden. And `seen`
    /// is **not** a snapshot taken at the time of the `idle` command: it is
    /// the reference the connection carries since its banner (see `versions`),
    /// so a change that occurred between two commands of this client is still
    /// ahead of it and comes out here.
    ///
    /// `Wakeup::versions` is what lets the caller advance its reference
    /// **subsystem by subsystem**: real MPD only clears the flags it has just
    /// reported, and advancing everything at once would lose the change of a
    /// subsystem not requested — the same error this one fixes, one notch
    /// further.
    ///
    /// The wakeup registration is done *under the read lock*, before the
    /// comparison. Without that the hole would reopen one notch further: a
    /// `notify_waiters` emitted between the comparison and the first poll of
    /// the `Notified` would find no registrant, and the sleeper would wait for
    /// the following change. A writer needs the write lock, so as long as the
    /// read guard is held, no change can slip between the registration and
    /// the comparison.
    ///
    /// The loop is not excess caution: `notify_waiters` wakes all sleepers,
    /// including those none of whose requested subsystems moved, and those
    /// must go back to sleep.
    ///
    /// Called by the session to hold an `idle`. An empty subsystem list never
    /// comes out of it, and that is the contract: see `Outcome::Wait`.
    pub async fn wait(&self, subsystems: &[Subsystem], seen: [u64; SUBSYSTEM_COUNT]) -> Wakeup {
        loop {
            let notified = self.wakeup.notified();
            tokio::pin!(notified);
            let (moved, versions) = {
                let inst = self.inner.read().await;
                // `enable` registers the future now rather than at the first
                // poll: see the reasoning about the lock above.
                let _ = notified.as_mut().enable();
                let moved = subsystems
                    .iter()
                    .copied()
                    .filter(|subsystem| inst.versions[*subsystem as usize] != seen[*subsystem as usize])
                    .collect::<Vec<_>>();
                // The counters of **this** read, and not of a second lock
                // acquisition afterwards: between the two, a reported
                // subsystem could move again, and the caller would advance its
                // reference past a change never announced.
                (moved, inst.versions)
            };
            if !moved.is_empty() {
                return Wakeup { moved, versions };
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog as the core emits one: every declared source is named, and
    /// its presets are those it knows how to enumerate (empty for the cd,
    /// which stays on the default body of `list_presets`).
    fn catalog_of(sources: &[(&str, &[(u8, &str)])]) -> SourcesCatalog {
        SourcesCatalog {
            sources: sources
                .iter()
                .map(|(name, presets)| SourceCatalog {
                    name: (*name).to_string(),
                    presets: presets
                        .iter()
                        .map(|(index, name)| Preset { index: *index, name: (*name).to_string() })
                        .collect(),
                })
                .collect(),
        }
    }

    /// The smallest catalog the core can emit: one named source, with one
    /// preset.
    fn single_source_catalog() -> SourcesCatalog {
        catalog_of(&[("radio", &[(1, "FIP")])])
    }

    #[tokio::test]
    async fn read_returns_the_default_state_before_anything_is_applied() {
        let shared = SharedState::default();
        assert_eq!(shared.read().await, Snapshot::default());
    }

    #[tokio::test]
    async fn apply_state_replaces_what_read_returns_next() {
        let shared = SharedState::default();
        let new_state = PlayerState { volume: 42, source: "radio".into(), ..Default::default() };

        shared.apply_state(new_state.clone()).await;

        assert_eq!(shared.read().await.state, new_state);
    }

    #[tokio::test]
    async fn a_frame_changing_the_volume_wakes_mixer_and_not_playlist() {
        let e = SharedState::default();
        let before = e.versions().await;
        e.apply_state(PlayerState { volume: 40, ..Default::default() }).await;
        let after = e.versions().await;
        assert_ne!(before[Subsystem::Mixer as usize], after[Subsystem::Mixer as usize]);
        assert_eq!(before[Subsystem::Playlist as usize], after[Subsystem::Playlist as usize]);
        assert_eq!(before[Subsystem::Player as usize], after[Subsystem::Player as usize], "the volume is not player's business");
    }

    #[tokio::test]
    async fn a_frame_changing_the_mute_wakes_mixer() {
        // `muted` counts as much as `volume`: MPD clients cut the sound by
        // sending `setvol 0`, but the mute can also come from the remote
        // control, and the client must learn it.
        let e = SharedState::default();
        let before = e.versions().await;
        e.apply_state(PlayerState { muted: true, ..Default::default() }).await;
        let after = e.versions().await;
        assert_ne!(before[Subsystem::Mixer as usize], after[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn an_identical_frame_wakes_nobody() {
        // The core already deduplicates, but a reconnection resends the
        // current state: it must not pass for a change.
        let e = SharedState::default();
        let frame = PlayerState {
            volume: 40,
            source: "radio".into(),
            playback: Playback::Playing,
            preset: Some(3),
            preset_count: Some(51),
            position_s: Some(12),
            ..Default::default()
        };
        e.apply_state(frame.clone()).await;
        let before = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_state(frame).await;

        assert_eq!(before, e.versions().await);
        assert_eq!(queue_version, e.read().await.queue_version, "the queue did not move either");
    }

    #[tokio::test]
    async fn a_source_change_wakes_playlist_and_player() {
        // The queue IS the preset list of the active source: changing source
        // changes the queue, and also changes what is playing.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let before = e.versions().await;

        e.apply_state(PlayerState { source: "cd".into(), ..Default::default() }).await;

        let after = e.versions().await;
        assert_ne!(before[Subsystem::Playlist as usize], after[Subsystem::Playlist as usize]);
        assert_ne!(before[Subsystem::Player as usize], after[Subsystem::Player as usize]);
        assert_eq!(before[Subsystem::Mixer as usize], after[Subsystem::Mixer as usize], "the volume did not move");
    }

    #[tokio::test]
    async fn an_inserted_disc_changes_the_queue() {
        // `preset_count` is the length of the MPD queue (`playlistlength`): an
        // inserted disc takes the CD player from "nothing to number" to twelve
        // tracks, without changing source name. Without a `Playlist` wakeup
        // nor a `queue_version` advance, a client stays on an empty queue and
        // the most ordinary action in the world is not seen from the phone.
        // And `Player` must not move: the queue changed, not what is playing.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "cd".into(), preset_count: Some(0), ..Default::default() })
            .await;
        let before = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_state(PlayerState { source: "cd".into(), preset_count: Some(12), ..Default::default() })
            .await;

        let after = e.versions().await;
        assert_ne!(before[Subsystem::Playlist as usize], after[Subsystem::Playlist as usize], "the queue changed");
        assert!(
            e.read().await.queue_version > queue_version,
            "queue_version must advance with the queue, otherwise plchanges will lie"
        );
        assert_eq!(before[Subsystem::Player as usize], after[Subsystem::Player as usize], "what is playing did not change");
        assert_eq!(before[Subsystem::Mixer as usize], after[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn every_preset_count_transition_moves_the_queue() {
        // The three transitions that the `Option<u8>` type makes distinct, at
        // constant source. The one carrying all the weight of this test is
        // **`None` -> `Some(0)`**: it is the only one a comparison written on
        // `unwrap_or(0)` would lose, and the two values do not describe the
        // same queue. `None` means "the source declared nothing" — the
        // consumer falls back on the historical 1-9 grid, hence nine entries;
        // `Some(0)` means "nothing to number" — a CD player without a disc,
        // hence zero entries. Confusing them would miss the insertion of a
        // disc in a source that declared nothing before.
        //
        // The other two rows cover both directions of the move, but one must
        // know what they are worth as proof: they would also pass under
        // `unwrap_or(0)` (0 != 12, then 12 != 0). Only the first one separates
        // the two implementations.
        let transitions: [(&str, Option<u8>, Option<u8>); 3] = [
            ("nothing declared -> zero tracks", None, Some(0)),
            ("zero tracks -> twelve tracks", Some(0), Some(12)),
            ("twelve tracks -> nothing declared", Some(12), None),
        ];
        for (name, from, to) in transitions {
            let e = SharedState::default();
            e.apply_state(PlayerState { source: "cd".into(), preset_count: from, ..Default::default() })
                .await;
            let before = e.versions().await;
            let queue_version = e.read().await.queue_version;

            e.apply_state(PlayerState { source: "cd".into(), preset_count: to, ..Default::default() })
                .await;

            let after = e.versions().await;
            assert_ne!(
                before[Subsystem::Playlist as usize],
                after[Subsystem::Playlist as usize],
                "{name}: the queue must move"
            );
            assert!(
                e.read().await.queue_version > queue_version,
                "{name}: queue_version must advance with the queue"
            );
            assert_eq!(
                before[Subsystem::Player as usize],
                after[Subsystem::Player as usize],
                "{name}: what is playing did not change"
            );
        }
    }

    #[tokio::test]
    async fn the_track_the_position_and_the_preset_wake_player_alone() {
        // The three fields the brief names under `player`, each tested
        // separately: forgetting one of the three would leave a client silent
        // for a whole track.
        let base = PlayerState { source: "radio".into(), ..Default::default() };
        let variants: [(&str, PlayerState); 3] = [
            ("playback", PlayerState { playback: Playback::Playing, ..base.clone() }),
            ("position", PlayerState { position_s: Some(7), ..base.clone() }),
            ("preset", PlayerState { preset: Some(4), ..base.clone() }),
        ];
        for (name, frame) in variants {
            let e = SharedState::default();
            e.apply_state(base.clone()).await;
            let before = e.versions().await;

            e.apply_state(frame).await;

            let after = e.versions().await;
            assert_ne!(before[Subsystem::Player as usize], after[Subsystem::Player as usize], "{name} should move player");
            assert_eq!(before[Subsystem::Playlist as usize], after[Subsystem::Playlist as usize], "{name} does not touch the queue");
            assert_eq!(before[Subsystem::Mixer as usize], after[Subsystem::Mixer as usize], "{name} does not touch the mixer");
        }
    }

    #[tokio::test]
    async fn the_position_clock_wakes_nobody() {
        // **The costliest regression of this plugin.** The core pushes one
        // frame per second during playback, and its only moving field is then
        // `position_s`: marking `Player` for that woke every client asleep on
        // `idle player` once per second. M.A.L.P. re-requested `status`,
        // `currentsong` and the **cover** at the same pace, which chopped up
        // the chunked transfer of the image — hence the instability and the
        // vanishing cover.
        let base = PlayerState {
            source: "files".into(),
            playback: Playback::Playing,
            position_s: Some(30),
            ..Default::default()
        };
        let e = SharedState::default();
        e.apply_state(base.clone()).await;
        let before = e.versions().await;

        // Four seconds of clock, one per frame: nothing must move.
        for s in 31..=34 {
            e.apply_state(PlayerState { position_s: Some(s), ..base.clone() }).await;
        }

        assert_eq!(
            before[Subsystem::Player as usize],
            e.versions().await[Subsystem::Player as usize],
            "time passing is not an MPD event"
        );
    }

    #[tokio::test]
    async fn a_seek_wakes_player() {
        // The counterpart of the test above: the tolerance must not swallow a
        // real seek, otherwise the client's progress bar would stay at the old
        // position until it re-requests `status` on its own. Both directions
        // count — a move backwards is always an event, an advance only beyond
        // the tolerance.
        let base = PlayerState {
            source: "files".into(),
            playback: Playback::Playing,
            position_s: Some(30),
            ..Default::default()
        };
        for (name, position) in [("forward", 90u32), ("backward", 5)] {
            let e = SharedState::default();
            e.apply_state(base.clone()).await;
            let before = e.versions().await;

            e.apply_state(PlayerState { position_s: Some(position), ..base.clone() }).await;

            assert_ne!(
                before[Subsystem::Player as usize],
                e.versions().await[Subsystem::Player as usize],
                "{name}: a seek must wake player"
            );
        }
    }

    #[tokio::test]
    async fn the_appearance_and_disappearance_of_the_position_wake_player() {
        // A track that starts, a stream that no longer has a position: two
        // different playback states, not the clock advancing.
        let without = PlayerState { source: "radio".into(), ..Default::default() };
        let with = PlayerState { position_s: Some(1), ..without.clone() };
        for (name, from, to) in
            [("appearance", without.clone(), with.clone()), ("disappearance", with, without)]
        {
            let e = SharedState::default();
            e.apply_state(from).await;
            let before = e.versions().await;

            e.apply_state(to).await;

            assert_ne!(
                before[Subsystem::Player as usize],
                e.versions().await[Subsystem::Player as usize],
                "{name}: must wake player"
            );
        }
    }

    #[tokio::test]
    async fn the_track_title_wakes_player() {
        // A radio stream changes neither source nor preset when the track
        // changes: it is the only signal the client will receive.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let before = e.versions().await;

        let mut frame = PlayerState { source: "radio".into(), ..Default::default() };
        frame.track.title = Some("Sonate".into());
        e.apply_state(frame).await;

        assert_ne!(before[Subsystem::Player as usize], e.versions().await[Subsystem::Player as usize]);
    }

    #[tokio::test]
    async fn no_frame_moves_stored_playlist() {
        // The only trigger of this subsystem is `apply_catalog`. A frame that
        // changes everything else must not increment it on the way.
        let e = SharedState::default();
        let before = e.versions().await;

        e.apply_state(PlayerState {
            volume: 30,
            muted: true,
            source: "cd".into(),
            playback: Playback::Playing,
            preset: Some(2),
            position_s: Some(3),
            ..Default::default()
        })
        .await;

        assert_eq!(
            before[Subsystem::StoredPlaylist as usize],
            e.versions().await[Subsystem::StoredPlaylist as usize]
        );
    }

    #[tokio::test]
    async fn a_new_catalog_wakes_stored_playlist() {
        let e = SharedState::default();
        let before = e.versions().await;

        e.apply_catalog(single_source_catalog()).await;

        let after = e.versions().await;
        assert_ne!(before[Subsystem::StoredPlaylist as usize], after[Subsystem::StoredPlaylist as usize]);
        assert_eq!(
            e.read().await.sources_catalog,
            single_source_catalog(),
            "the catalog must also be memorised, not only counted"
        );
    }

    #[tokio::test]
    async fn an_identical_catalog_wakes_nobody() {
        // The core sends the current value **at connection time**: a
        // reconnection of the `display` half comes through here again with
        // the same catalog, and must not pass for a change — otherwise every
        // restart of the plugin wakes all clients.
        let e = SharedState::default();
        e.apply_catalog(single_source_catalog()).await;
        let before = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_catalog(single_source_catalog()).await;

        assert_eq!(before, e.versions().await);
        assert_eq!(queue_version, e.read().await.queue_version);
    }

    #[tokio::test]
    async fn a_catalog_touching_the_active_source_also_moves_the_queue() {
        // The MPD queue *is* the preset list of the active source: renaming a
        // radio station while it is playing changes the queue, hence
        // `Playlist` and `queue_version` with it.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        e.apply_catalog(catalog_of(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let before = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_catalog(catalog_of(&[("radio", &[(1, "FIP Rock")]), ("cd", &[])])).await;

        let after = e.versions().await;
        assert_ne!(before[Subsystem::StoredPlaylist as usize], after[Subsystem::StoredPlaylist as usize]);
        assert_ne!(before[Subsystem::Playlist as usize], after[Subsystem::Playlist as usize]);
        assert!(
            e.read().await.queue_version > queue_version,
            "queue_version must advance with the queue, otherwise plchanges will lie"
        );
    }

    #[tokio::test]
    async fn a_catalog_touching_only_an_inactive_source_leaves_the_queue_alone() {
        // The counterpart, and the one with value: the radio renames a station
        // while the cd is playing. The stored playlists changed, the queue did
        // not — waking `Playlist` would make every client re-download the
        // queue, and `plchanges` would return an identical queue under a new
        // version.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "cd".into(), ..Default::default() }).await;
        e.apply_catalog(catalog_of(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let before = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_catalog(catalog_of(&[("radio", &[(1, "FIP"), (5, "Nova")]), ("cd", &[])]))
            .await;

        let after = e.versions().await;
        assert_ne!(
            before[Subsystem::StoredPlaylist as usize],
            after[Subsystem::StoredPlaylist as usize],
            "the stored playlists did change"
        );
        assert_eq!(
            before[Subsystem::Playlist as usize],
            after[Subsystem::Playlist as usize],
            "the queue comes from the active source, and it did not move"
        );
        assert_eq!(queue_version, e.read().await.queue_version);
        assert_eq!(
            before[Subsystem::Player as usize],
            after[Subsystem::Player as usize],
            "a catalog says nothing about what is playing"
        );
    }

    #[tokio::test]
    async fn the_catalog_does_not_travel_with_every_state_frame() {
        // Non-regression of the two-channel choice: ten state frames, a single
        // catalog. The frames are all different (the volume rises), so each
        // one does wake something — this test cannot pass merely because there
        // was nothing to apply.
        let e = SharedState::default();
        e.apply_catalog(single_source_catalog()).await;
        let after_catalog = e.versions().await;
        for v in 1..=10u8 {
            e.apply_state(PlayerState { volume: v, ..Default::default() }).await;
        }
        let after = e.versions().await;
        assert_eq!(after[Subsystem::StoredPlaylist as usize], after_catalog[Subsystem::StoredPlaylist as usize]);
        assert_eq!(
            after[Subsystem::Mixer as usize],
            after_catalog[Subsystem::Mixer as usize] + 10,
            "the ten frames must have counted for ten, otherwise this test proves nothing"
        );
        assert_eq!(e.read().await.sources_catalog, single_source_catalog(), "and the catalog survives the frames");
    }

    #[tokio::test]
    async fn source_catalog_distinguishes_an_unknown_name_from_an_empty_list() {
        // The distinction `listplaylistinfo` and `load` rest on: a name absent
        // from the catalog is an `ACK 50`, a known name without presets is an
        // empty, well-formed response.
        let e = SharedState::default();
        e.apply_catalog(catalog_of(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let inst = e.read().await;

        assert!(inst.source_catalog("whatever").is_none());
        assert_eq!(inst.source_catalog("cd").map(|s| s.presets.len()), Some(0));
        assert_eq!(inst.source_catalog("radio").map(|s| s.presets.len()), Some(1));
    }

    #[tokio::test]
    async fn active_presets_follow_the_source_the_frame_names() {
        // `active_presets` reads the source name of the last frame: it is the
        // frame and not the catalog that says what is playing.
        let e = SharedState::default();
        e.apply_catalog(catalog_of(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;

        e.apply_state(PlayerState { source: "cd".into(), ..Default::default() }).await;
        assert!(e.read().await.active_presets().is_empty());

        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        assert_eq!(e.read().await.active_presets().len(), 1);
    }

    #[tokio::test]
    async fn a_catalog_wakes_a_sleeper_registered_on_stored_playlists() {
        // The useful counterpart: a client sleeping on `stored_playlist` must
        // return when the catalog arrives, and it is the only event that will
        // ever wake it.
        let e = std::sync::Arc::new(SharedState::default());
        let seen = e.versions().await;
        let sleeper = {
            let e = e.clone();
            tokio::spawn(async move { e.wait(&[Subsystem::StoredPlaylist], seen).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        e.apply_catalog(single_source_catalog()).await;

        assert_eq!(sleeper.await.unwrap().moved, vec![Subsystem::StoredPlaylist]);
    }

    #[tokio::test]
    async fn the_queue_version_is_monotonic() {
        // Never reset to zero: a comparing client would believe it missed
        // nothing. The third round comes back to "radio", the initial value,
        // and that is precisely the case an implementation derived from the
        // state (and not from a counter) would miss.
        let e = SharedState::default();
        let mut previous = e.read().await.queue_version;
        for source in ["radio", "cd", "radio"] {
            e.apply_state(PlayerState { source: source.into(), ..Default::default() }).await;
            let v = e.read().await.queue_version;
            assert!(v > previous, "{v} should exceed {previous}");
            previous = v;
        }
    }

    #[tokio::test]
    async fn the_queue_version_only_moves_when_the_queue_moves() {
        // The counterpart of the previous test: monotonic does not mean "rising
        // at every frame". A `plchanges` would otherwise return the whole
        // queue at every second of playback.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let before = e.read().await.queue_version;

        e.apply_state(PlayerState { source: "radio".into(), volume: 50, position_s: Some(9), ..Default::default() })
            .await;

        assert_eq!(before, e.read().await.queue_version);
    }

    #[tokio::test]
    async fn a_change_that_occurred_before_the_wait_is_not_lost() {
        // THE test that matters: the session reads the versions, a change
        // arrives, *then* it goes to sleep. It must return at once. With a
        // `Notify` alone, that wakeup would be lost and the client would stay
        // silent until the next change.
        let e = SharedState::default();
        let seen = e.versions().await;
        e.apply_state(PlayerState { volume: 40, ..Default::default() }).await;
        // No `timeout` here: if the wait blocks, the test hangs and the failure
        // is frank. A clock margin would be a flake in waiting.
        let changes = e.wait(&[Subsystem::Mixer], seen).await;
        assert_eq!(changes.moved, vec![Subsystem::Mixer]);
        // And the wakeup returns the counters that decided it: this is what
        // the session will keep as the new reference of its connection.
        assert_eq!(changes.versions, e.versions().await);
    }

    #[tokio::test]
    async fn the_wait_only_returns_the_requested_subsystems() {
        let e = SharedState::default();
        let seen = e.versions().await;
        e.apply_state(PlayerState { volume: 40, source: "cd".into(), ..Default::default() }).await;
        let changes = e.wait(&[Subsystem::Mixer], seen).await;
        assert_eq!(changes.moved, vec![Subsystem::Mixer], "playlist changed but was not requested");
    }

    #[tokio::test]
    async fn the_wait_returns_the_subsystems_in_the_requested_order() {
        // The order is that of the request and not that of the enum: it is
        // what the session will write as `changed:` lines, and a stable order
        // is what makes that output testable at Task 8.
        let e = SharedState::default();
        let seen = e.versions().await;
        e.apply_state(PlayerState { volume: 40, source: "cd".into(), ..Default::default() }).await;

        let changes = e.wait(&[Subsystem::Playlist, Subsystem::Mixer, Subsystem::Player], seen).await;

        assert_eq!(changes.moved, vec![Subsystem::Playlist, Subsystem::Mixer, Subsystem::Player]);
    }

    #[tokio::test]
    async fn a_frame_arriving_during_the_wait_wakes_the_sleeper() {
        // The other half of the mechanism: when the preliminary comparison
        // finds nothing, the `Notify` must be what returns. The sleeper is
        // launched in a task and the `yield_now`s let it reach its wait point
        // (single-threaded scheduler of `#[tokio::test]`, so the queued task
        // runs before the one that yields).
        //
        // No clock: if the notification does not arrive, the `await` on the
        // handle hangs and the failure is frank. A "long enough" `timeout`
        // would be a flake in waiting — exactly the family of tests the
        // previous effort had to delete.
        let e = std::sync::Arc::new(SharedState::default());
        let seen = e.versions().await;
        let sleeper = {
            let e = e.clone();
            tokio::spawn(async move { e.wait(&[Subsystem::Player], seen).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;

        assert_eq!(sleeper.await.unwrap().moved, vec![Subsystem::Player]);
    }

    #[tokio::test]
    async fn a_sleeper_does_not_return_on_a_subsystem_that_is_not_its_own() {
        // `notify_waiters` wakes everyone, so a sleeper registered on `Mixer`
        // alone is indeed woken by a `player` frame — and must go back to
        // sleep. Without the loop in `wait`, it would return an empty list and
        // the session would write an `OK` without `changed:`, which no MPD
        // client knows how to interpret.
        let e = std::sync::Arc::new(SharedState::default());
        let seen = e.versions().await;
        let sleeper = {
            let e = e.clone();
            tokio::spawn(async move { e.wait(&[Subsystem::Mixer], seen).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        // Only `player` moves: the sleeper is woken for nothing.
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!sleeper.is_finished(), "a wakeup on another subsystem must not end the wait");

        // Then what it was really waiting for.
        e.apply_state(PlayerState { playback: Playback::Playing, volume: 22, ..Default::default() }).await;
        assert_eq!(sleeper.await.unwrap().moved, vec![Subsystem::Mixer]);
    }

    #[tokio::test]
    async fn the_optimistic_state_precedes_the_frame_then_yields_to_it() {
        // The `pause` race: the plugin acknowledges the toggle as soon as it
        // emits it, and the next frame is authoritative.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        e.acknowledge_optimistic(&[Command::PlayPause]).await;
        assert_eq!(e.read().await.playback(), Playback::Paused, "acknowledged before the frame");
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        assert_eq!(e.read().await.playback(), Playback::Playing, "the frame is authoritative");
    }

    #[tokio::test]
    async fn the_optimistic_toggle_starts_from_the_optimistic_value() {
        // Two `pause` in a row come back to the starting state: the toggle
        // reads `optimistic_playback` and not the frame, otherwise the second
        // one would toggle again from `Playing` and still return `Paused`.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;

        e.acknowledge_optimistic(&[Command::PlayPause]).await;
        e.acknowledge_optimistic(&[Command::PlayPause]).await;

        assert_eq!(e.read().await.playback(), Playback::Playing);
    }

    #[tokio::test]
    async fn the_optimistic_toggle_has_no_effect_when_stopped() {
        // `PlayPause` when stopped starts a playback of which the plugin knows
        // neither what nor where: it waits for the frame rather than
        // announcing `Playing` on an empty track.
        let e = SharedState::default();
        let before = e.versions().await;

        e.acknowledge_optimistic(&[Command::PlayPause]).await;

        assert_eq!(e.read().await.playback(), Playback::Stopped);
        assert_eq!(before, e.versions().await, "nothing to announce, hence no wakeup");
    }

    #[tokio::test]
    async fn acknowledging_a_volume_publishes_it_at_once_and_wakes_mixer() {
        // A client sending `setvol 70` then `status` in the same breath must
        // read 70, and the other clients must be woken: the confirming frame,
        // for its part, will be identical and move nothing.
        let e = SharedState::default();
        let before = e.versions().await;

        e.acknowledge_optimistic(&[Command::SetVolume(70)]).await;

        assert_eq!(e.read().await.state.volume, 70);
        assert_ne!(before[Subsystem::Mixer as usize], e.versions().await[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn acknowledging_the_volume_already_in_place_wakes_nobody() {
        let e = SharedState::default();
        e.apply_state(PlayerState { volume: 70, ..Default::default() }).await;
        let before = e.versions().await;

        e.acknowledge_optimistic(&[Command::SetVolume(70)]).await;

        assert_eq!(before, e.versions().await);
    }

    #[tokio::test]
    async fn acknowledging_ignores_commands_whose_effect_cannot_be_guessed() {
        // Guessing what a `Select` does to the position, the track or the
        // preset would be wrong more often than right: the active source
        // decides.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, volume: 30, ..Default::default() }).await;
        let snapshot_before = e.read().await;

        // `Mute` is **no longer** in the list: it is acknowledged since
        // `setvol` unmutes (see `commands::setvol`), and its own test is right
        // below. `VolumeUp`/`VolumeDown` stay here: they carry no value and
        // the core decides the step.
        e.acknowledge_optimistic(&[
            Command::Select(4),
            Command::Next,
            Command::Prev,
            Command::Stop,
            Command::SeekTo(30),
            Command::VolumeUp,
            Command::SourceCycle,
        ])
        .await;

        assert_eq!(snapshot_before, e.read().await, "none of these commands is acknowledged");
    }

    #[tokio::test]
    async fn a_list_of_two_toggles_counts_as_a_single_change() {
        // The deduplication of `mark`: two `pause` in a single MPD command
        // list pass under the lock once, and a single change is published —
        // the final state, for its part, is indeed that of the two toggles.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        let before = e.versions().await;

        e.acknowledge_optimistic(&[Command::PlayPause, Command::PlayPause]).await;

        assert_eq!(
            before[Subsystem::Player as usize] + 1,
            e.versions().await[Subsystem::Player as usize],
            "a single increment for a single lock acquisition"
        );
        assert_eq!(e.read().await.playback(), Playback::Playing);
    }

    #[tokio::test]
    async fn acknowledging_mute_toggles_the_mute_and_wakes_mixer() {
        // Without this acknowledgement, the unmuting `setvol` would be
        // invisible: `status` publishes `volume: 0` as long as `state.muted`
        // is true, so a client raising its slider would see it fall back to
        // zero until the next frame — the exact defect the optimistic layer
        // exists to avoid.
        let e = SharedState::default();
        e.apply_state(PlayerState { volume: 40, muted: true, ..Default::default() }).await;
        let before = e.versions().await;

        e.acknowledge_optimistic(&[Command::SetVolume(40), Command::Mute]).await;

        let inst = e.read().await;
        assert!(!inst.state.muted, "the mute must be lifted");
        assert_eq!(inst.state.volume, 40);
        assert_ne!(before[Subsystem::Mixer as usize], e.versions().await[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn acknowledging_mute_is_indeed_a_toggle_in_both_directions() {
        // A toggle and not a set: acknowledging it as "muted = true" would
        // make a `Mute` emitted from an already muted device publish a mute
        // that the core has, on the contrary, just lifted.
        let e = SharedState::default();
        e.acknowledge_optimistic(&[Command::Mute]).await;
        assert!(e.read().await.state.muted, "false -> true");
        e.acknowledge_optimistic(&[Command::Mute]).await;
        assert!(!e.read().await.state.muted, "true -> false");
    }

    #[test]
    fn the_subsystems_index_the_array_without_gaps() {
        // The design rests on `subsystem as usize`: should a variant one day
        // receive an out-of-bounds or duplicate value, the indexing would
        // panic or two subsystems would share a counter.
        let indices = [
            Subsystem::Player as usize,
            Subsystem::Mixer as usize,
            Subsystem::Playlist as usize,
            Subsystem::StoredPlaylist as usize,
        ];
        let mut seen = [false; SUBSYSTEM_COUNT];
        for i in indices {
            assert!(i < SUBSYSTEM_COUNT, "{i} falls outside the counter array");
            assert!(!seen[i], "two subsystems share index {i}");
            seen[i] = true;
        }
        assert!(seen.iter().all(|v| *v), "an index of the array has no subsystem");
    }

    // ------------------------------------------------------------------
    // Covers
    // ------------------------------------------------------------------

    /// The `href` the core publishes, in the two places that must coincide:
    /// the state frame and the cover frame.
    const HREF: &str = "/api/cover/1a2b3c";

    #[tokio::test]
    async fn a_received_cover_is_held_and_wakes_player() {
        let e = SharedState::default();
        let before = e.versions().await;

        e.apply_cover(test_cover(HREF, 4096)).await;

        let inst = e.read().await;
        let held = inst.cover.expect("the cover must be held");
        assert_eq!(held.href, HREF);
        assert_eq!(held.mime, "image/jpeg");
        // The bytes to the bit: this is what `albumart` will serve.
        assert_eq!(*held.bytes, test_cover(HREF, 4096).bytes);
        assert_ne!(
            before[Subsystem::Player as usize],
            e.versions().await[Subsystem::Player as usize],
            "a cover is a fact about the current track"
        );
    }

    #[tokio::test]
    async fn a_cover_wakes_only_player() {
        // The counterpart of the previous test: `Mixer` has nothing to do with
        // an image, and waking `Playlist` would make every client re-download
        // the whole queue at every track change. `StoredPlaylist` is reserved
        // for stored playlists.
        let e = SharedState::default();
        let before = e.versions().await;
        let queue_before = e.read().await.queue_version;

        e.apply_cover(test_cover(HREF, 4096)).await;

        let after = e.versions().await;
        for subsystem in [Subsystem::Mixer, Subsystem::Playlist, Subsystem::StoredPlaylist] {
            assert_eq!(
                before[subsystem as usize], after[subsystem as usize],
                "{subsystem:?} has nothing to learn from a cover"
            );
        }
        // And neither does the queue version: the queue did not change, and
        // incrementing it would make `plchanges` answer for nothing.
        assert_eq!(queue_before, e.read().await.queue_version);
    }

    #[tokio::test]
    async fn the_same_cover_twice_wakes_nobody() {
        // The core pushes the current cover **at wiring time**, so a
        // reconnection of the `display` half comes through here again with
        // the same image. Without the comparison, every restart of the plugin
        // would wake all clients — and make them re-download up to twenty
        // mebibytes.
        let e = SharedState::default();
        e.apply_cover(test_cover(HREF, 4096)).await;
        let before = e.versions().await;

        e.apply_cover(test_cover(HREF, 4096)).await;

        assert_eq!(before, e.versions().await);
    }

    #[tokio::test]
    async fn different_bytes_under_the_same_href_are_a_change() {
        // The comparison is on the bytes and not only on the `href`: trusting
        // the key alone would silence a really new image published under a
        // recycled key, and the client would keep the old one forever.
        let e = SharedState::default();
        e.apply_cover(test_cover(HREF, 4096)).await;
        let before = e.versions().await;

        e.apply_cover(test_cover(HREF, 8192)).await;

        assert_ne!(before[Subsystem::Player as usize], e.versions().await[Subsystem::Player as usize]);
        assert_eq!(e.read().await.cover.unwrap().bytes.len(), 8192);
    }

    /// A state frame **as the core emits it when a cover exists**: it
    /// announces the `href` of the held image.
    ///
    /// The realism is not a courtesy. This test used a `Default` frame — hence
    /// without `cover_href` — to prove that a state frame does not throw the
    /// cover away: a frame the producer **never** emits at the same time as a
    /// cover, and which therefore proved an impossible causality. It hid, on
    /// the way, that nothing ever released the image.
    fn frame_announcing(href: &str) -> PlayerState {
        PlayerState {
            source: "radio".into(),
            preset: Some(2),
            track: ritornello_proto::Track {
                title: Some("So What".into()),
                cover_href: Some(href.to_string()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_state_frame_does_not_throw_away_the_cover_it_announces() {
        // The two channels write into the same snapshot and each must touch
        // only its own — the same property as for the catalog. A state frame
        // arrives **every second** of playback: if it reset the cover to
        // `None`, `albumart` would only answer between two frames, i.e. never.
        let e = SharedState::default();
        e.apply_cover(test_cover(HREF, 4096)).await;

        e.apply_state(PlayerState { volume: 17, ..frame_announcing(HREF) }).await;

        let inst = e.read().await;
        assert_eq!(inst.cover.map(|p| p.bytes.len()), Some(4096));
        assert_eq!(inst.state.volume, 17);
    }

    #[tokio::test]
    async fn a_frame_without_a_cover_releases_the_held_bytes() {
        // The counterpart, and it was entirely missing: `cover` was never
        // reset to `None`, so the plugin kept up to 20 MiB for the life of the
        // process — including long after playback stopped. The signal is the
        // `cover_href` of the state frame: `None` means nothing playing has an
        // image any more, and that is exactly the condition under which
        // `albumart` was already refusing to serve those bytes.
        let e = SharedState::default();
        e.apply_state(frame_announcing(HREF)).await;
        e.apply_cover(test_cover(HREF, 4096)).await;
        assert!(e.read().await.cover.is_some(), "the cover must first be held");

        // The next track has no artwork: the core announces it so, and will
        // send no cover frame for it.
        e.apply_state(PlayerState {
            track: ritornello_proto::Track { title: Some("Blue in Green".into()), ..Default::default() },
            ..frame_announcing(HREF)
        })
        .await;

        assert!(e.read().await.cover.is_none(), "the bytes must be released");
    }

    #[tokio::test]
    async fn a_frame_announcing_another_key_keeps_the_held_cover() {
        // The core's normal window: it sends the state **before** the bytes,
        // so the frame already announces the next key while the held cover is
        // still the previous one. The release must not trigger there —
        // otherwise an inversion of the two channels' order would destroy an
        // image that the next frame would have legitimised. `albumart` refuses
        // during that window (the `href` does not match), and that is all
        // that is needed.
        let e = SharedState::default();
        e.apply_state(frame_announcing(HREF)).await;
        e.apply_cover(test_cover(HREF, 4096)).await;

        e.apply_state(frame_announcing("/api/cover/999999")).await;

        assert!(e.read().await.cover.is_some(), "the state/cover window is not a release");
    }

    #[tokio::test]
    async fn a_sleeper_on_player_is_woken_by_a_cover() {
        // The end-to-end of the wakeup, within this module: `wait` does not
        // poll, so it is indeed the `notify_waiters` of `apply_cover` that
        // returns. No clock: if the implementation did not wake, this test
        // **would hang** — the intended failure mode.
        let e = Arc::new(SharedState::default());
        let seen = e.versions().await;
        let sleeper = e.clone();
        let waiting = tokio::spawn(async move { sleeper.wait(&[Subsystem::Player], seen).await });
        // The preliminary comparison in `wait` forbids the missed wakeup:
        // whether the cover arrives before or after the sleeper registers, it
        // returns. No synchronisation is therefore needed here.
        e.apply_cover(test_cover(HREF, 4096)).await;
        assert_eq!(waiting.await.unwrap().moved, vec![Subsystem::Player]);
    }
}
