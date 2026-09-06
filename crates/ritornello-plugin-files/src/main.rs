//! Source `files`: plays audio files from a local root or a mounted network
//! share.
//!
//! mpv holds the playback list: the plugin hands it a generated m3u and drives
//! the index. Automatic advance therefore goes through `playlist-pos`, exactly
//! as for a disc, and the plugin has nothing to pace itself.
//!
//! Two independent halves, on the model of the radio plugin: the Source and the
//! admin page, each in its own task, sharing the roots table and the current
//! playlist. A failure of the page must never cut the audio.

mod admin;
mod archive;
mod cover;
mod state;

use anyhow::Result;
use rand::seq::SliceRandom;
use ritornello_i18n::Catalog;
use ritornello_plugin_files::m3u::Entry;
use ritornello_plugin_files::playlist::Playlist;
use ritornello_plugin_files::roots::{RootKind, Roots};
use ritornello_plugin_files::FILES_EN;
use ritornello_plugin_sdk::{Notification, Runtime, SourceOutcome, SourcePlugin};
use ritornello_proto::{Preset, SourceAction};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Where a drawn playback order comes from.
///
/// One type for both play modes: `set_play_mode` draws a fresh order when
/// shuffle engages, and `end_of_content` draws again when repeat-all opens a
/// new pass. Both go through the same `draw` call, so a real shuffle
/// (`Random`) must hand back a different permutation each time it is asked —
/// which is exactly what `Sequence` below lets a test prove: an injected
/// queue of permutations shows that a second pass is drawn again and not
/// replayed, something no statistical check on a real RNG could show.
// `main()` only ever builds `Order::Random`: the other three variants are
// constructed by test code alone (see `source_with`), so a plain (non-test)
// build sees them as unconstructed — same situation as
// `ritornello-core::placeholder`.
#[allow(dead_code)]
enum Order {
    /// Positions and entries coincide: `0, 1, 2, …`. What `FilesSource` falls
    /// back to whenever shuffle is off — the identity permutation, not a
    /// distinct code path.
    Sequential,
    /// A single permutation, injected in tests that only ever open one pass:
    /// every draw hands back the same vector.
    Fixed(Vec<usize>),
    /// A queue of permutations, injected in tests that must open a second
    /// pass and prove it was drawn again, not replayed: each draw pops the
    /// next entry. Falls back to the identity permutation once exhausted,
    /// rather than panicking on a test that happens to draw once too often.
    Sequence(Vec<Vec<usize>>),
    /// Production: a fresh Fisher-Yates shuffle against the thread RNG at
    /// every draw.
    Random,
}

impl Order {
    /// Hands back a permutation of `0..len`, consuming one draw.
    fn draw(&mut self, len: usize) -> Vec<usize> {
        match self {
            Order::Sequential => (0..len).collect(),
            Order::Fixed(order) => order.clone(),
            Order::Sequence(queue) => {
                if queue.is_empty() {
                    (0..len).collect()
                } else {
                    queue.remove(0)
                }
            }
            Order::Random => {
                let mut order: Vec<usize> = (0..len).collect();
                order.shuffle(&mut rand::rng());
                order
            }
        }
    }
}

/// What `arm_cover`'s per-directory memo says about a folder.
///
/// **Three named states and not two nested `Option`s.** `NotProbed` is the one
/// that matters: a share that timed out stores nothing, and reading "unknown"
/// as "no image" would archive over a cover nobody has looked for. Written
/// `Option<Option<CoverRef>>`, that distinction is one missing `.flatten()`
/// away from disappearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderState {
    NotProbed,
    NoImage,
    Image,
}

/// Should this Source offer to keep a network cover for `file`?
///
/// A free function, and not a method: it is a decision over three values, so
/// it is provable without a `FilesSource`, without a channel and without a
/// clock.
fn offers_archive(roots: &Roots, file: &Path, probed: FolderState) -> bool {
    if probed != FolderState::NoImage {
        return false;
    }
    let Some(root) = roots.root_of(file) else { return false };
    if !root.archive_covers {
        return false;
    }
    // `writable` gates the cifs mount options, so it only means something for
    // a share. A local root is writable or not according to its filesystem,
    // which answers at write time (and is refused with a line in the journal).
    match root.kind {
        RootKind::Smb => root.writable,
        RootKind::Local => true,
    }
}

struct FilesSource {
    /// Shared with the Admin half, which modifies it from the page.
    playlist: Arc<AsyncRwLock<Playlist>>,
    /// The page has modified the playlist since we handed it to mpv.
    ///
    /// mpv plays a **copy**, written at the last `Play`. Any modification
    /// drifts away from it, and the Admin half has no way to tell mpv: the
    /// SDK's notifications deliberately carry no action. This flag is therefore
    /// the only channel, and it is used at the next command received — that is
    /// where a fresh playlist can legitimately be handed back to mpv.
    playlist_changed: Arc<std::sync::atomic::AtomicBool>,
    /// Are we playing right now. Read by the page (see `plays` on the Admin side).
    plays: Arc<std::sync::atomic::AtomicBool>,
    /// The order mpv actually walks: mpv position `i` plays `playlist`
    /// entry `order[i]`. A permutation of `0..entries.len()`, identity when
    /// shuffle is off — so `position_of`/`entry_at` never need a separate
    /// "not shuffling" case.
    ///
    /// Deliberately **not** carried by `Playlist`: the admin page mutates
    /// `entries`/`index` at seven places (add, remove, three cases of move,
    /// clear, load), each with its own arithmetic, and an order stored there
    /// would need repairing at every one of them. Instead it lives here and
    /// is redrawn wholesale from `playlist_changed` (see `play`,
    /// `reload_if_changed`) — a modification mid-pass restarts the pass,
    /// which is the accepted price of not keeping seven arithmetics correct.
    order: Vec<usize>,
    /// Where the next `order` comes from: a real shuffle in production
    /// (`Order::Random`, built in `main()`), an injected permutation or
    /// queue of permutations in tests. Kept across draws, not thrown away
    /// after the first one, because a repeated pass under repeat-all must
    /// draw again rather than replay the same permutation.
    draw: Order,
    /// The two play modes, learned from `set_play_mode` and consulted by
    /// `end_of_content`/`activate` to know whether a finished pass should
    /// open another one.
    random: bool,
    repeat_all: bool,
    /// Set by `end_of_content` when it was called without repeat-all — which
    /// only happens once mpv's list has run to its end. The next `Play` then
    /// opens a fresh pass instead of replaying the single entry left at the
    /// tail of the exhausted order — see `activate`'s doc for the defect this
    /// avoids.
    pass_finished: bool,
    /// `set_play_mode` engaged or disengaged shuffle since the last reload,
    /// while a file may already have been playing under mpv's own m3u.
    ///
    /// Read (and cleared) by `reload_if_changed`, alongside — but distinct
    /// from — `playlist_changed` above: reusing that flag would draw
    /// `order` a **second** time, since `set_play_mode` already drew its
    /// own (see its doc), desyncing an injected `Order::Sequence` queue
    /// from the passes it is meant to describe.
    ///
    /// Regression #4 (whole-branch review): `set_play_mode` has no action
    /// to return (see the trait's doc), so without a flag of its own, a
    /// mode toggled while a file was already playing never reached mpv at
    /// all — mpv kept walking its old file at its old position while
    /// `order` here already described a different one, and the very next
    /// natural advance translated mpv's reported position through the
    /// *new* order onto the *old* file: a wrong title, identity and cover
    /// displayed while the sound stayed sequential, until whatever later
    /// reload finally caught up.
    mode_changed: bool,
    state_path: PathBuf,
    /// The **generated** m3u that mpv receives. Decoupled from any user playlist.
    mpv_playlist_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
    /// Preset count announced by the Admin half after every modification of
    /// the playlist.
    ///
    /// `main()` always builds this field as `Some`: the admin page is
    /// registered unconditionally with `Runtime`. `None` only appears in the
    /// tests, which build `FilesSource` directly without going through
    /// `Runtime`, hence without an Admin half to emit on this channel;
    /// `poll_notification` then stays pending forever rather than returning
    /// `None`, which is **terminal** for the SDK.
    preset_count_rx: Option<tokio::sync::watch::Receiver<u8>>,
    /// In-flight result of the cover lookup for the armed track.
    ///
    /// **Carried by an independent `tokio::spawn` task**, launched by
    /// `arm_cover` — and not by a direct call to `health.bounded(...).await`
    /// here.
    /// A first version made that call directly in `poll_notification`, which
    /// is the future that the SDK's `select!` cancels as soon as a request
    /// from the core arrives during the wait — a common event, not an edge
    /// case. A cancellation before `health`'s timeout expires skips its `Err`
    /// arm: nothing is marked silent, the inner `spawn_blocking` task is
    /// simply detached (Tokio cancels nothing on drop), and since the armed
    /// track is deliberately not forgotten on cancellation, the next call
    /// relaunched a **second** probe on the same stuck share — one more
    /// `spawn_blocking` thread per cycle, where `health.rs` promises at most
    /// one abandoned thread per mount point. By taking the probe out of this
    /// cancellable loop, it always runs to completion exactly once, and
    /// `health`'s bookkeeping stays exact.
    ///
    /// `oneshot::Receiver::await` is documented cancel-safe: if
    /// `poll_notification` is cancelled during the wait, this receiver — kept
    /// here and not in a local variable of the future — has lost nothing, and
    /// the next call resumes waiting on the same in-flight task rather than
    /// launching another one.
    ///
    /// A new `Play` while a probe is in flight replaces this field with a
    /// fresh receiver: the old one is dropped, and the result of the old task
    /// — when it eventually arrives — will land in a `send` with nobody
    /// listening. This is deliberate: a cover of the previous track must
    /// never announce itself for the track that has just started.
    cover_in_flight: Option<tokio::sync::oneshot::Receiver<Option<ritornello_proto::CoverRef>>>,
    /// Cover remembered **per directory**: the probed directory, and what was
    /// found there (`None` in second position = probed, nothing certain).
    ///
    /// The directory and not the file, because that is the granularity of the
    /// thing being looked for: a `folder.jpg` belongs to the album, not to the
    /// track. This is what allows the cover to be re-announced at **every**
    /// identity declaration — mpv's automatic advance included, which goes
    /// through `player_track`/`resync` and not through `play()` — without
    /// paying a `readdir` on an SMB share again at every track. Without this
    /// re-announcement, a ripped album showed its cover on track 1 and the ♫
    /// fallback on the following ones: the core clears `cover_source` on
    /// every identity change (see `Metadata::set_identity`), and only `play()`
    /// re-armed the probe.
    ///
    /// Shared with the probe task, which writes it when it finishes, hence the
    /// `Arc<Mutex<…>>`. A single directory remembered: only one is listened to
    /// at a time, and going back in the playlist costs only one `readdir`.
    // Type deliberately left as the covers project wrote it.
    // `clippy::type_complexity` rejects it, and a named alias would have been
    // the fix the rule suggests — but naming it required documenting its
    // meaning, hence interpreting the semantics of another project's double
    // `Option` from a merge commit it did not review. A wrong statement placed
    // next to someone else's code is worse than a silenced rule: the rule, at
    // least, is honest about what it is, whereas the comment reads as
    // knowledge. The field doc above is theirs, and it is enough.
    #[allow(clippy::type_complexity)]
    cover_by_dir: Arc<Mutex<Option<(PathBuf, Option<ritornello_proto::CoverRef>)>>>,
    /// Circuit breaker for media paths, shared with the Admin half.
    ///
    /// The `read_dir` of the cover lookup targets a share that may stay silent
    /// indefinitely (see `health`): without this bound, a sleeping NAS would
    /// freeze the probe task above indefinitely.
    health: Arc<ritornello_plugin_files::health::Health>,
    /// The roots table, shared with the Admin half, which is the only writer.
    ///
    /// Consulted by `poll_notification` alone, through `offers_archive`, to
    /// learn whether the root that owns the current file welcomes a network
    /// cover (`archive_covers`, and for a share, `writable`). Nothing here
    /// ever touches a filesystem: `offers_archive` reads this table and the
    /// `cover_by_dir` memo, never a directory.
    roots: Arc<AsyncRwLock<Roots>>,
    /// The file whose identity was last declared, set at the top of
    /// `arm_cover` regardless of which of its three branches is taken.
    ///
    /// `poll_notification` needs it once the probe answers, to resolve which
    /// root — hence which `archive_covers` — the offer would be about: the
    /// `path` local of the probe itself lives only inside the spawned task,
    /// and is gone by the time its result reaches us.
    current_file: Option<PathBuf>,
}

impl FilesSource {
    /// Identity of what is playing: the file, designated by its absolute path.
    ///
    /// Opaque to the core, which only compares and relays it. It is also what
    /// a `metadata` plugin would read to recognise a track.
    fn identity(path: &Path) -> serde_json::Value {
        serde_json::json!({ "kind": "file", "path": path.to_string_lossy() })
    }

    /// Redraws `order` for the playlist's current length: identity when not
    /// shuffling — position and entry then coincide, and `entry_at`/
    /// `position_of` need no separate case for it — a fresh permutation from
    /// `self.draw` otherwise.
    async fn draw_order(&mut self) {
        let len = self.playlist.read().await.entries.len();
        self.order = if self.random { self.draw.draw(len) } else { (0..len).collect() };
    }

    /// If the page modified the playlist since the last draw, redraws
    /// `order` for its new length and clears the flag. Idempotent: called a
    /// second time right after, it finds the flag already clear and does
    /// nothing — which is what lets both `play` and `reload_if_changed` call
    /// it without drawing twice for one modification (a second draw would
    /// desync a `Order::Sequence` queue from the passes it is meant to
    /// describe).
    async fn resync_order_if_changed(&mut self) {
        if self.playlist_changed.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.draw_order().await;
        }
    }

    /// mpv position `position` -> the playlist entry that plays there.
    fn entry_at(&self, position: usize) -> Option<usize> {
        self.order.get(position).copied()
    }

    /// Playlist entry `entry` -> the mpv position that plays it.
    ///
    /// Linear search: `order` holds one small playlist's worth of positions
    /// (a device's local library, not a streaming catalogue), and is walked
    /// at most once per command — a reverse index would be premature here.
    fn position_of(&self, entry: usize) -> Option<usize> {
        self.order.iter().position(|&e| e == entry)
    }

    /// The order currently handed to mpv, for tests that must tell a fresh
    /// draw from a replayed one apart.
    #[allow(dead_code)] // read only from test code (see `source_with`'s callers)
    fn drawn_order(&self) -> &[usize] {
        &self.order
    }

    fn phrase(&self, key: &str) -> String {
        self.catalog.read().unwrap().get(key).to_string()
    }

    /// Permanent status of the source.
    ///
    /// **Redeclared on every meaningful frame**: `status` has the opposite
    /// convention to `preset`, absence meaning "no status" and not "keep the
    /// previous one". A Source that omitted it would see its display erase
    /// itself at the next frame.
    fn status(&self) -> String {
        self.phrase("status_files")
    }

    async fn persist(&self) {
        let index = self.playlist.read().await.index;
        // `update` and not `save`: the Admin half writes the playlist into this
        // same file, and a `save` rebuilt here would erase it. The failure is
        // logged and not propagated — a read-only `/var/lib` must cost the
        // resume after reboot, not the playback in progress.
        if let Err(e) = state::update(&self.state_path, |s| s.index = index) {
            tracing::warn!("persisting the current track: {e}");
        }
    }

    /// Arms the announcement of the cover of `file`'s directory.
    ///
    /// To be called from **every** path that declares an identity: the core
    /// resets its cover on every identity change, so an identity declared
    /// without re-announcement is a lost cover.
    ///
    /// Two cases, and that is the whole point of remembering:
    /// - the directory is the one already probed — the case of the vast
    ///   majority of track changes, an album being a directory — and the
    ///   answer leaves **immediately**, without any disk access;
    /// - the directory changes: we probe, once.
    ///
    /// The probe remains carried by an independent `tokio::spawn` task with a
    /// `oneshot`, and this is not a matter of style (see the doc of
    /// `cover_in_flight`): the SDK's `select!` cancels `poll_notification` as
    /// soon as a request from the core arrives, and a call to
    /// `health.bounded(...)` made from that future would lose the circuit
    /// breaker's bookkeeping. The remembered path goes through the same
    /// `oneshot`, already filled: nothing new to cancel, and above all **no
    /// path by which `poll_notification` could return `None`**, which is
    /// terminal for the SDK — an `Err` from the receiver as well as an
    /// `Ok(None)` both fall through to the rest of the function.
    fn arm_cover(&mut self, file: &Path) {
        // Set unconditionally, ahead of the three branches below: whichever
        // one answers, `poll_notification` must find here the file this
        // very call was about (see the field's doc).
        self.current_file = Some(file.to_path_buf());
        // A fresh receiver replaces the one of a probe still in flight: this is
        // what discards the cover of a track already left (see the doc of
        // `cover_in_flight`).
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cover_in_flight = Some(rx);
        let Some(dir) = file.parent().map(Path::to_path_buf) else {
            let _ = tx.send(None);
            return;
        };
        if let Some((known, found)) = &*self.cover_by_dir.lock().unwrap()
            && known == &dir
        {
            // `debug` and not `info`: `arm_cover` is called twice per
            // track (the playback, then the resync), so this line came
            // out twice **per track** for a fact that does not change for
            // the whole album. The fresh lookup below, on the other hand,
            // stays at `info`: once per directory, it is the useful
            // answer to "why no cover".
            if found.is_none() {
                tracing::debug!("no cover file in {} (remembered)", dir.display());
            }
            let _ = tx.send(found.clone());
            return;
        }
        let health = self.health.clone();
        let memory = self.cover_by_dir.clone();
        let path = file.to_path_buf();
        tokio::spawn(async move {
            let to_search = path.clone();
            match health.bounded(&path, move || cover::search(&to_search)).await {
                Some(found) => {
                    match &found {
                        Some(_) => tracing::info!("cover file found in {}", dir.display()),
                        None => tracing::info!("no cover file in {}", dir.display()),
                    }
                    // Remembered even when nothing was found: this is what
                    // avoids re-probing a directory without an image at every
                    // track.
                    *memory.lock().unwrap() = Some((dir, found.clone()));
                    let _ = tx.send(found);
                }
                // The circuit breaker could not tell (silent share, timeout
                // elapsed): **nothing is remembered**. Retaining "no cover"
                // here would condemn this directory for the whole session on
                // the sole word of a momentarily sleeping NAS, whereas `health`
                // precisely hands control back as soon as it answers again.
                None => {
                    // A real incident — it is the silent share that `health`
                    // exists to bound — hence `warn`, and not the previous
                    // silence.
                    tracing::warn!("cover lookup in {} gave up: share not answering", dir.display());
                    let _ = tx.send(None);
                }
            }
            // Ignored if nobody is listening any more (track changed since):
            // this is the very mechanism that discards a stale result.
        });
    }

    /// Starts the playlist at the current index, after rewriting mpv's m3u.
    async fn play(&mut self) -> SourceOutcome {
        // A pass that just ended is over and done with: whatever brought us
        // here (a fresh activation, a selection) is a new listening session,
        // and `pass_finished`'s only reader is `activate`, which reads it
        // before calling here (see its doc).
        self.pass_finished = false;
        // Whatever reload was owed for a mode change is fulfilled by this
        // very call, however it was reached — directly (`activate`,
        // `select`) or through `reload_if_changed`'s own check. Left set,
        // the next `next`/`prev`/`player_track` would reload a second time
        // for nothing.
        self.mode_changed = false;
        // We hand mpv the playlist as it is now: the drift is closed, whatever
        // its cause was — `order` included, redrawn here if the page changed
        // the playlist since the last draw.
        self.resync_order_if_changed().await;
        let playlist = self.playlist.read().await;
        let count = playlist.preset_count();
        let Some(entry) = playlist.current().cloned() else {
            self.plays.store(false, std::sync::atomic::Ordering::Relaxed);
            return SourceOutcome::new(SourceAction::Noop)
                .status(self.phrase("no_playlist"))
                .preset_count(0)
                .plays_nothing();
        };
        self.plays.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = playlist.write_for_mpv(&self.mpv_playlist_path, &self.order) {
            tracing::warn!("writing the mpv playlist: {e}");
        }
        let index = playlist.index;
        let preset = playlist.preset();
        drop(playlist);
        // Armed here, read later by `poll_notification`. On a real `Play`
        // only — the empty playlist left above through the `return`, so we
        // never probe for nothing.
        self.arm_cover(&entry.path);

        // The **mpv position**, not the entry index: `order` may have moved
        // this entry away from its own position in the displayed list. Falls
        // back to `index` itself if it is somehow missing from `order` (it
        // should not be, `order` being a permutation of the same length) —
        // starting somewhere is better than a panic here.
        let start = self.position_of(index).unwrap_or(index);
        let action = SourceAction::play(self.mpv_playlist_path.to_string_lossy().to_string())
            // Without this declaration, the core would load the m3u as a
            // single media: mpv would only expand it afterwards, the starting
            // index would arrive out of bounds, and every track selection
            // would replay the first one while losing the display. Measured,
            // and fixed here.
            .playlist()
            .starting_at(start as i64)
            // A list of files has a normal end: without this declaration,
            // mpv's inactivity at the end of the list would pass for a stream
            // cut and the restart would replay the list in a loop.
            .finite();
        let mut outcome = SourceOutcome::new(action)
            .plays(Self::identity(&entry.path))
            .preset_name(entry.display_name())
            .preset_count(count)
            .status(self.status());
        if let Some(n) = preset {
            outcome = outcome.preset(n);
        }
        outcome
    }

    /// If the page has modified the playlist, hands it back to mpv shifted by
    /// `step`.
    ///
    /// `None` when nothing has changed: the caller then delegates to mpv, as
    /// before. Without this resync, next/previous walked the playlist that mpv
    /// held at the last `Play` — tracks added since were out of reach, and
    /// those removed came back.
    ///
    /// The shift starts from **our** index, which the Admin half keeps up to
    /// date as it modifies the playlist; mpv's, on the other hand, designates a
    /// position in a stale list.
    async fn reload_if_changed(&mut self, step: i64) -> Option<SourceOutcome> {
        // Distinct from `playlist_changed` (see `mode_changed`'s own doc),
        // but read through the exact same `step` arithmetic below: a mode
        // toggled mid-playback needs the same "move on from wherever the
        // playing entry now sits in the (possibly new) order" reasoning a
        // playlist edit already gets. Cleared as a side effect of the
        // `play()` this function always ends with below, not here — so an
        // `activate`/`select` that reaches `play()` some other way clears
        // it too, without a second, redundant reload the next time this
        // function runs.
        if !self.playlist_changed.load(std::sync::atomic::Ordering::Relaxed) && !self.mode_changed {
            return None;
        }
        // Redrawn here, and not only inside `play()` below: the shift a few
        // lines down is computed in **position** space, from `order`, which
        // must already reflect the playlist's new length before that
        // arithmetic runs. `resync_order_if_changed` is idempotent, so
        // `play()`'s own call further down finds the flag already clear and
        // does not draw a second time for this one modification.
        self.resync_order_if_changed().await;
        {
            let mut playlist = self.playlist.write().await;
            if playlist.entries.is_empty() {
                // Nothing to play: `play()` will say so, and there is no index
                // to move.
                drop(playlist);
                return Some(self.play().await);
            }
            let n = self.order.len() as i64;
            // The step walks **mpv positions**, not playlist entries: with
            // shuffle engaged, `index + step` names a different track than
            // "the next one mpv would play". `position_of` finds where the
            // current entry sits in the drawn order, the step moves within
            // that order, and `entry_at` translates the landing position
            // back to an entry.
            let position = self.position_of(playlist.index).unwrap_or(0) as i64;
            // Wrap around at the bounds, as mpv does from one end of its own
            // list to the other: the user must not end up stuck because a
            // modification left them on the last track.
            let next_position = (((position + step) % n + n) % n) as usize;
            playlist.index = self.entry_at(next_position).unwrap_or(playlist.index);
        }
        Some(self.play().await)
    }

    /// Frame that only restates where we are, without relaunching anything.
    ///
    /// "Without relaunching anything" on the audio side; on the cover side, on
    /// the contrary, it **re-announces**. This frame declares an identity, and
    /// the core clears what it held on every identity change: this is the
    /// path of mpv's automatic advance, hence of every track of an album
    /// except the one the user launched themselves. The probe is only paid
    /// again if the directory changes (see `arm_cover`).
    async fn resync(&mut self) -> SourceOutcome {
        let playlist = self.playlist.read().await;
        let mut outcome = SourceOutcome::new(SourceAction::Noop)
            .preset_count(playlist.preset_count())
            .status(self.status());
        let mut file = None;
        if let Some(entry) = playlist.current() {
            outcome = outcome.plays(Self::identity(&entry.path)).preset_name(entry.display_name());
            file = Some(entry.path.clone());
        }
        if let Some(n) = playlist.preset() {
            outcome = outcome.preset(n);
        }
        drop(playlist);
        if let Some(file) = file {
            self.arm_cover(&file);
        }
        outcome
    }
}

#[async_trait::async_trait]
impl SourcePlugin for FilesSource {
    async fn activate(&mut self) -> SourceOutcome {
        // The index is kept: resuming after a stop returns the track we were
        // listening to, and not the first one.
        //
        // An earlier version restarted from the beginning when the playlist had
        // ended, trusting mpv's `playlist-pos = -1`. Measured: this -1
        // **also arrives transiently at every playlist reload**, hence at
        // every track change. The resume then fell back on track 1. The signal
        // being unreliable, the distinction is abandoned rather than guessed —
        // at the cost of one detail: after a playlist that ran to its end, the
        // Play key replays the last track.
        //
        // **Except when the last pass genuinely ran out** (`pass_finished`,
        // set by `end_of_content` without repeat-all): the entry the kept
        // index designates then sits at the very tail of the exhausted
        // order, with nothing drawn after it. Replaying from there would
        // give exactly one track before mpv goes idle again — a fresh pass
        // is opened instead, drawn again under shuffle.
        if self.pass_finished {
            if self.random {
                self.draw_order().await;
            }
            if let Some(entry) = self.entry_at(0) {
                self.playlist.write().await.index = entry;
            }
        }
        self.play().await
    }

    async fn deactivate(&mut self) -> SourceOutcome {
        self.plays.store(false, std::sync::atomic::Ordering::Relaxed);
        SourceOutcome::new(SourceAction::Stop).plays_nothing().status(self.status())
    }

    async fn select(&mut self, n: u8) -> SourceOutcome {
        if self.playlist.write().await.select(n) {
            self.persist().await;
            return self.play().await;
        }
        // Nothing was launched: the previous track is still playing. Transient
        // message, and above all **no identity declaration** — a
        // `plays_nothing()` here would make the `metadata` plugins stop and
        // would blank the displayed title while the sound goes on.
        let count = self.playlist.read().await.preset_count();
        SourceOutcome::new(SourceAction::Noop)
            .status(self.phrase("empty_track"))
            .transient()
            .preset_count(count)
    }

    async fn next(&mut self) -> SourceOutcome {
        // The playlist changed under mpv: hand it the new one, positioned on
        // the following track. This is the legitimate moment to do it — an
        // explicit command from the user, who expects a track change.
        if let Some(outcome) = self.reload_if_changed(1).await {
            return outcome;
        }
        // Otherwise mpv walks its own list; it is mpv that will tell us where
        // it landed, through `player_track`. Nothing to resync here, on pain
        // of doing it twice and contradicting ourselves.
        SourceOutcome::new(SourceAction::PlayerNext).status(self.status())
    }

    async fn prev(&mut self) -> SourceOutcome {
        if let Some(outcome) = self.reload_if_changed(-1).await {
            return outcome;
        }
        SourceOutcome::new(SourceAction::PlayerPrev).status(self.status())
    }

    async fn eject(&mut self) -> SourceOutcome {
        // Nothing to eject: no removable media here.
        SourceOutcome::new(SourceAction::Noop).status(self.status())
    }

    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        // mpv has just moved to the next track **on its own**. If the playlist
        // changed since it received it, this is the best moment to hand it the
        // new one: it is starting a file anyway, so nothing is interrupted —
        // whereas waiting for an explicit command let playback chain on in the
        // old list, and that is exactly what usage showed as "the
        // modifications do nothing".
        //
        // Only for a valid index: at `-1` the playlist has ended, and reloading
        // would restart it instead of letting it finish.
        if n >= 0 {
            // The shift starts from **our** index — the track that has just
            // ended — so "the next one" is read in the up-to-date playlist.
            if let Some(outcome) = self.reload_if_changed(1).await {
                return outcome;
            }
        }
        // `n` is an **mpv position**, not a playlist entry index: shuffled,
        // the two diverge, and setting the raw position as the index would
        // have the display name the wrong track (see `entry_at`'s doc on
        // `FilesSource`). A negative `n` has no position to translate and
        // falls through unchanged, which `set_index` below discards exactly
        // as it always has.
        let entry = usize::try_from(n)
            .ok()
            .and_then(|pos| self.entry_at(pos))
            .map(|e| e as i64)
            .unwrap_or(n);
        if !self.playlist.write().await.set_index(entry) {
            // mpv says `-1` at the end of the list — **and also transiently at
            // every playlist reload**, hence at every track change: this is
            // measured, and that is why no conclusion is drawn from it.
            // Declare nothing; the eventual stop will be announced by `stop()`.
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.persist().await;
        self.resync().await
    }

    async fn stop(&mut self) -> SourceOutcome {
        // The core stopped on its own initiative, or the playlist ended. Say
        // so, otherwise the last track and its metadata would stay displayed
        // indefinitely.
        //
        // And **say which of the three**, which was not the case: this frame
        // overwrote the "NO PLAYLIST" that `play()` had just displayed. Without
        // a track, mpv stays idle, the core therefore sends `stop()` at once,
        // and the user only saw a generic status without ever learning that
        // their playlist was empty.
        self.plays.store(false, std::sync::atomic::Ordering::Relaxed);
        let playlist = self.playlist.read().await;
        if playlist.entries.is_empty() {
            return SourceOutcome::new(SourceAction::Noop)
                .plays_nothing()
                .status(self.phrase("no_playlist"))
                .preset_count(0);
        }
        // **Stopped, but a track armed.** The old frame only announced a
        // status: the display lost number and name, and showed nothing more
        // than "FILES" without any way to know where we were. Declaring the
        // current track without a playback identity says exactly the real
        // state — nothing is playing, and here is what will restart.
        let mut outcome = SourceOutcome::new(SourceAction::Noop)
            .plays_nothing()
            .status(self.status())
            .preset_count(playlist.preset_count());
        if let Some(entry) = playlist.current() {
            outcome = outcome.preset_name(entry.display_name());
        }
        if let Some(n) = playlist.preset() {
            outcome = outcome.preset(n);
        }
        outcome
    }

    /// A list of files is exactly the kind of finite list random/repeat-all
    /// are for — unlike the radio, which has none. A constant, not derived
    /// from whether the playlist currently holds anything: an empty playlist
    /// still has a shape to shuffle or repeat once tracks are added, the same
    /// way an empty disc tray still opens (see `can_eject`'s doc).
    fn has_finite_list(&self) -> bool {
        true
    }

    /// Learns the two play modes together (see `SourcePlugin::set_play_mode`
    /// for why they travel as one call).
    ///
    /// Only draws (and repositions) on an actual **transition** of `random`.
    ///
    /// The core rediffuses this request to every source whenever *either*
    /// mode changes, and also at wake-up and hot-plug — so most calls land
    /// here with `random` unchanged. Redrawing unconditionally (an earlier
    /// version, caught in review) silently overwrote `order` and moved the
    /// index without ever rewriting mpv's m3u or emitting an action: mpv kept
    /// walking its own file at its own position while `order` came to
    /// describe a different one, so every later translation through it
    /// (`player_track`, `next`, `prev`) reasoned about a list mpv did not
    /// have — the screen naming the wrong track, next/prev landing at the
    /// wrong position, with nothing to see in any single frame.
    async fn set_play_mode(&mut self, random: bool, repeat_all: bool) {
        let random_changed = random != self.random;
        self.random = random;
        self.repeat_all = repeat_all;
        if !random_changed {
            return;
        }
        // A mode change that actually (dis)engages shuffle opens a pass, in
        // the sense that matters here: the "finished" state from a previous
        // pass no longer describes anything once shuffle has just been
        // toggled.
        self.pass_finished = false;
        // Identity when `random` just turned false, so a mode change never
        // leaves a stale shuffled order behind once shuffle is turned back
        // off.
        self.draw_order().await;
        if random {
            if self.plays.load(std::sync::atomic::Ordering::Relaxed) {
                // Regression #4b (whole-branch review): a mode toggled
                // **while a file is already playing** must not jump the
                // pass to a fresh, unrelated first entry — that track is
                // already sounding under mpv's old m3u, and the eventual
                // reload (armed below by `mode_changed`) reaches whatever
                // entry it should land on through the exact `step`
                // arithmetic a playlist edit already uses in
                // `reload_if_changed`: it moves **on from** wherever
                // `index` sits in `order`, by `step`. Pointing `index` at
                // the drawn pass's own first entry (the branch below, kept
                // for the case nothing was playing yet) only to have that
                // same reload immediately step past it
                // (`step == 1` for `next`/`player_track`) would skip that
                // first entry outright — it was never actually played.
                // Moving the entry **already playing** to the head of the
                // draw instead needs no such correction: the reload's
                // `step` of `1` then lands on the pass's real *second*
                // entry, which is exactly what should follow it.
                let current = self.playlist.read().await.index;
                if let Some(position) = self.position_of(current)
                    && position != 0
                {
                    self.order.swap(0, position);
                }
            } else if let Some(entry) = self.entry_at(0) {
                // Nothing playing yet: start the freshly drawn pass at its
                // own beginning. Without this, engaging shuffle over an
                // idle playlist would leave the index wherever it had last
                // been left — a position that means nothing in the new
                // order, and could replay, later in this same pass, a
                // track already heard before shuffle was even turned on.
                self.playlist.write().await.index = entry;
            }
        }
        // Regression #4 (whole-branch review): this method has no action to
        // return (see the trait's doc), so a mode toggled while a file was
        // already playing under mpv's own m3u would otherwise never reach
        // mpv at all — only a page edit (`playlist_changed`) used to arm a
        // reload. See `mode_changed`'s own doc on the struct for what that
        // silently produced, and why this cannot simply reuse
        // `playlist_changed`. Read (and cleared) by `reload_if_changed`,
        // itself read from `next`/`prev`/`player_track` — the same three
        // places a playlist edit's own reload is read from, matching the
        // design's promise of a reload "at the next track change".
        self.mode_changed = true;
    }

    /// The core got the full-size original of a cover for a track whose folder
    /// has none, and left it at `file`. See `SourceReq::ArchiveCover` for why
    /// `identity` **designates** a folder rather than gating a comparison, and
    /// `archive::store` for every reason this hand-over may end in a refusal.
    ///
    /// **Returns at once, and works detached.** The reply is what unties a
    /// correlation the core gives up on after five seconds, while a copy onto a
    /// sleeping share takes longer than that; a Source that copied before
    /// answering would routinely be told it failed when it did not. Nothing
    /// below is awaited by the core.
    async fn archive_cover(&mut self, identity: serde_json::Value, file: String) {
        let roots = self.roots.clone();
        let health = self.health.clone();
        tokio::spawn(async move {
            let staged = PathBuf::from(&file);
            let table = roots.read().await.clone();
            // The circuit breaker is keyed on the mount point owning the path
            // it is handed, and what this work touches is the **album folder**
            // — not the staged file, which sits in the appliance's own tmpfs
            // and would charge a sleeping NAS's timeout to the wrong mount.
            // An echo we cannot read names no folder; the staged path then
            // makes an honest key for a call that will refuse without ever
            // touching a share.
            let guarded = archive::echoed_file(&identity).unwrap_or_else(|| staged.clone());
            // **One** `Health::bounded` for the whole folder: listing it,
            // reading the neighbours' albums and writing the image are one trip
            // to one share, and a sleeping NAS must give up once, not once per
            // neighbour.
            let target = staged.clone();
            // **Read before `bounded`, and that order is the whole point.**
            // `bounded` answers `None` for two situations that call for
            // opposite treatments here, and only this reading tells them
            // apart: a mount *already* known silent means the closure never
            // ran, while an elapsed bound means it is still running and owns
            // both the staged file and the duty to report. Read afterwards,
            // the flag would be true in both cases — the timeout marks the
            // mount on its way out — and the wrong branch would be taken for
            // the one case that must not be touched. A mount that falls
            // silent *between* these two lines leaves one staged file in the
            // appliance's tmpfs; that is a leak, where the reverse order
            // risks truncating a cover being written into a music library.
            let already_silent = health.unreachable(&guarded);
            // **The outcome is logged from inside the closure**, not from the
            // result of `bounded`. An elapsed bound hands the `JoinHandle` to
            // the recovery watcher, which discards what the closure returned:
            // reporting from out here would leave every slow attempt — the
            // successes included — with no line at all, which is precisely
            // the silence this feature is not allowed to produce.
            let work = move || {
                match archive::store(&table, &identity, &target, &archive::album_from_tags) {
                    archive::Outcome::Written(p) => {
                        tracing::info!("cover archived at {}", p.display())
                    }
                    archive::Outcome::Refused(why) => {
                        tracing::info!("cover not archived: {why}")
                    }
                    archive::Outcome::Failed(e) => tracing::warn!("cover not archived: {e}"),
                }
            };
            if health.bounded(&guarded, work).await.is_none() {
                if already_silent {
                    // Nothing ran, so nothing said anything and nothing reaped
                    // the staged file. Both are owed here: the file is ours
                    // from the moment the path arrived, the core never comes
                    // back for it.
                    tracing::warn!(
                        "cover not archived: {} is not answering",
                        guarded.display()
                    );
                    let _ = std::fs::remove_file(&staged);
                } else {
                    // The bound elapsed — the share has not said no, it has
                    // not said anything *yet*. The abandoned thread still
                    // holds the staged file and will report for itself, so
                    // this must neither claim an outcome nor unlink under it.
                    tracing::warn!(
                        "cover not archived yet: {} did not answer within the bound; \
                         the attempt continues in the background and will report itself",
                        guarded.display()
                    );
                }
            }
        });
    }

    /// mpv went idle at the end of the finite list `has_finite_list`
    /// declares — as opposed to a live stream cutting out, which has no
    /// equivalent here.
    ///
    /// Without repeat-all, behaves like the default `stop()` fallback always
    /// did — but remembers that the pass is over, for `activate` to read (see
    /// its doc). With repeat-all, opens a new pass: drawn again under
    /// shuffle, replayed identically otherwise, starting at that pass's
    /// first entry either way.
    async fn end_of_content(&mut self) -> SourceOutcome {
        if !self.repeat_all {
            self.pass_finished = true;
            return self.stop().await;
        }
        if self.random {
            self.draw_order().await;
        }
        if let Some(entry) = self.entry_at(0) {
            self.playlist.write().await.index = entry;
        }
        self.play().await
    }

    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() =
            Catalog::load("files", &locale, &self.locales_root, FILES_EN);
    }

    /// The named presets, for the home page grid and for the sources_catalog
    /// that the core keeps for the displays.
    ///
    /// Without this override, the trait's default body returned an empty
    /// list: the source only declared a `preset_count`, and the grid tiles
    /// carried only a number where the radio shows "1 · FIP". It is the same
    /// route the radio takes, and the sources_catalog already distinguishes
    /// "I only have numbers" (empty list) from "here are my names".
    async fn list_presets(&mut self) -> Vec<Preset> {
        self.playlist.read().await.presets()
    }

    async fn poll_notification(&mut self) -> Option<Notification> {
        // A probe is in flight: wait for its result, without ever relaunching
        // it from this future (see the field doc — it is `play()` that
        // launches it, on a task that this cancellation does not touch).
        //
        // `rx.await` and not `rx.try_recv()`: it is precisely the wait itself
        // that must survive the cancellation of `poll_notification`, not be
        // worked around. `oneshot::Receiver` documents its `.await` as
        // cancel-safe, and the receiver lives in `self` — not in a local
        // variable of this future — so nothing is lost if this round is
        // interrupted: the next one resumes waiting on the same task.
        //
        // **And nothing suspends between that answer and the `return` below**,
        // which is the other half of the same guarantee. The SDK cancels this
        // future the instant a core request arrives; once `cover_in_flight`
        // has been cleared, the received cover exists only in this future's
        // locals, so a single `.await` in the lines that follow would drop it
        // with nothing left to resume — the cover would never be announced for
        // that track, and nothing would say so. That is why the offer below is
        // computed with `try_read` and not `read().await`.
        if let Some(rx) = &mut self.cover_in_flight {
            let result = rx.await;
            // Cleared only after the probe has answered — this is what makes
            // the guarantee above true: as long as no answer has arrived, the
            // field stays in place for the next round.
            self.cover_in_flight = None;
            // `Err` (the task vanished without answering) and `Ok(None)` (the
            // circuit breaker said "we don't know", or the lookup itself said
            // "nothing certain") both mean "no cover to announce", but they no
            // longer mean "nothing at all to say": a folder the memo confirms
            // was probed and holds no image may still be worth an offer to
            // archive one. The channel cannot tell that case apart from a
            // share that timed out — both send `None` — so this reads the
            // `cover_by_dir` memo instead, which is where `arm_cover` records
            // the difference (see `FolderState`'s doc).
            //
            // `self.cover_by_dir.lock()` and `self.current_file` are both read
            // here, in their own statement: the `std::sync::MutexGuard` this
            // produces is not `Send`, and this function suspends again further
            // down (`rx.changed().await`) when there is nothing to announce.
            // Computing `probed` first lets the guard drop at the end of this
            // statement, before any of that.
            let probed = match &result {
                Ok(Some(_)) => FolderState::Image,
                _ => match (&self.current_file, &*self.cover_by_dir.lock().unwrap()) {
                    (Some(file), Some((dir, None))) if file.parent() == Some(dir.as_path()) => {
                        FolderState::NoImage
                    }
                    _ => FolderState::NotProbed,
                },
            };
            let mut n = Notification::new();
            if let Ok(Some(cover)) = &result {
                n = n.cover(cover.clone());
            }
            // `try_read` and **never** `read().await`: see the cancel-safety
            // note at the top of this function. The lock is only ever held for
            // writing while the Admin half saves a table, so the failure is
            // rare — and losing an offer costs nothing that does not come
            // back, since the next track probes its folder and offers again,
            // where losing the cover received two statements above would be
            // silent and permanent.
            let offer = match &self.current_file {
                Some(file) => self
                    .roots
                    .try_read()
                    .map(|table| offers_archive(&table, file, probed))
                    .unwrap_or(false),
                None => false,
            };
            if offer {
                n = n.cover_archivable(true);
            }
            // A frame carrying neither a cover nor an offer says nothing, and
            // the SDK's "interesting frame" predicate would drop it anyway:
            // returning it would only cost a wake-up. Above all not `None`
            // here either way — that is terminal for the SDK (see the comment
            // on `preset_count_rx` just below) — so a frame with nothing to
            // say simply falls through to the rest of the function, which
            // waits for the next event.
            if matches!(result, Ok(Some(_))) || offer {
                return Some(n);
            }
        }
        let Some(rx) = &mut self.preset_count_rx else {
            // Only happens in tests (see the comment on the field): `main()`
            // always builds this receiver. Never `None` here, which would be
            // terminal for the SDK.
            return std::future::pending().await;
        };
        match rx.changed().await {
            Ok(()) => {
                let n = *rx.borrow_and_update();
                // The count, **and the number and name of the current track**.
                //
                // The number alone was not enough: reordering the playlist
                // changes the position of what we are listening to, and the
                // display's counter stayed on the old one — the plugin page
                // was right, the player was not. It is the exact counterpart
                // of the radio's fix, where the preset is also a position.
                //
                // Still **no identity and no action**: the current track must
                // be neither interrupted nor redeclared, only renumbered.
                //
                // Caution: the core **does not merge** `status`, contrary to
                // what this place claimed. `preset`, `preset_name` and
                // `preset_count` are indeed kept when absent — this is what
                // makes this partial notice legitimate — but `status` is
                // *replaced* by what the frame carries, absence included
                // (`Core::handle_source_update`: `if !update.transient
                // { self.source_status = update.status.clone(); }`). It is the
                // only convention that allows a status to be erased.
                //
                // This notice therefore declares none **and erases none**: the
                // core returns early, before that processing, for a frame that
                // declares neither identity nor status. Without that guard —
                // and this was the case in service — saving a playlist from
                // this page blanked the source's status on the console and the
                // SPA until the next command.
                let playlist = self.playlist.read().await;
                let mut notice = Notification::new().preset_count(n);
                // The **names**, republished with the count. The channel wakes
                // up on every modification of the playlist (`watch::send`
                // signals even with an equal value), so a mere reordering —
                // which does not change the count — still renames the tiles.
                // Without this, the grid would have kept the previous titles
                // under the new numbers, which is worse than no title.
                //
                // Nothing is published for an empty playlist: absence is what
                // says "I only have numbers" (see `SourceOutcome::presets`),
                // and an empty list would be indistinguishable there from a
                // deliberate erasure of the sources_catalog.
                let presets = playlist.presets();
                if !presets.is_empty() {
                    notice = notice.presets(presets);
                }
                if let Some(entry) = playlist.current() {
                    notice = notice.preset_name(entry.display_name());
                }
                if let Some(p) = playlist.preset() {
                    notice = notice.preset(p);
                }
                Some(notice)
            }
            // The sender is gone (Admin half terminated): nothing more to
            // announce, but the Source keeps playing.
            Err(_) => std::future::pending().await,
        }
    }
}

/// True for a frame that this plugin agrees to write to the log.
///
/// **It discards only one thing: `lofty`'s chatter below the error level.**
/// The durations survey opens the header of every file of the playlist, and
/// `lofty` emits a `WARN` there for every MP3 without a Xing header —
/// "MPEG: Using bitrate to estimate duration". This is not an incident: it is
/// the normal estimation method for that format, it calls for no action, and
/// it repeats per track. Reported by the owner as polluting his log, and the
/// cost is real: the core only retains the `WARN` and above lines for the
/// "last errors" card, so that noise pushes real errors out of the buffer.
///
/// `lofty` keeps its `ERROR`s: a frame that the library deems faulty remains
/// information.
///
/// A `filter_fn` and not an `EnvFilter`: the latter lives behind the optional
/// `env-filter` feature of `tracing-subscriber`, which pulls in `regex` — one
/// more dependency to compile and to ship on a Pi, for a single rule known in
/// advance.
fn frame_to_log(metadata: &tracing::Metadata<'_>) -> bool {
    // `>` and not `<`: in `tracing`, the order of the levels is that of
    // verbosity, so `ERROR` is the **smallest**. "More verbose than error" is
    // indeed written `> Level::ERROR`.
    !(metadata.target().starts_with("lofty") && *metadata.level() > tracing::Level::ERROR)
}

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(tracing_subscriber::filter::filter_fn(frame_to_log))
        .init();

    let state_path =
        PathBuf::from(env_or("RITORNELLO_FILES_STATE", "/var/lib/ritornello/plugin-files.json"));
    let mpv_playlist_path = PathBuf::from(env_or(
        "RITORNELLO_FILES_MPV_PLAYLIST",
        "/var/lib/ritornello/plugin-files.m3u",
    ));
    let roots_path =
        PathBuf::from(env_or("RITORNELLO_FILES_ROOTS", "/etc/ritornello/media-roots.toml"));
    let creds_dir = PathBuf::from(env_or(
        "RITORNELLO_FILES_CREDENTIALS",
        "/etc/ritornello/media-credentials",
    ));
    let playlists_dir =
        PathBuf::from(env_or("RITORNELLO_FILES_PLAYLISTS", "/var/lib/ritornello/playlists"));
    // Transient working directory, where the network wizard drops its
    // authentication file for the duration of an `smbclient` call.
    //
    // The **runtime directory**, and above all not the persisted credentials
    // one: that one lives under `/etc` and is only writable in production.
    // Confusing the two made the wizard fail in development with a
    // "Permission denied" that seemed to blame SMB.
    //
    // Same default and same variable as the core (`RITORNELLO_RUNTIME_DIR`), so
    // that `docs/development.md` stays true from one binary to the other.
    let runtime_dir = PathBuf::from(env_or("RITORNELLO_RUNTIME_DIR", "/run/ritornello"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));

    let state = state::load(&state_path);
    let entries: Vec<Entry> = state.playlist.iter().map(Entry::from).collect();
    // Missing tracks are **kept**: a momentarily unreachable share (sleeping
    // NAS, mount not yet done at boot) would otherwise erase the user's
    // playlist.
    //
    // And above all: they are **not counted here**. This count used to be done
    // by an `is_file` on every track, before the two halves were launched —
    // hence before the admin socket existed. On 2026-08-17, a cifs mount stuck
    // in the kernel held up the start there, and the management page simply
    // vanished from the UI: the core only sees an admin plugin once it has
    // bound its socket. Nothing that touches a media path may run before that.
    // The page returns the same information, under the circuit breaker,
    // through the `missing` field of `get_data`.
    let index = if state.index < entries.len() { state.index } else { 0 };

    let roots = Roots::load(&roots_path).unwrap_or_else(|e| {
        tracing::warn!("no usable media-roots.toml ({e}): starting with no root");
        Roots::default()
    });
    let catalog = Arc::new(RwLock::new(Catalog::load("files", "en", &locales_root, FILES_EN)));
    // Captured before the move below: the Source's initial `order` is the
    // identity permutation of this same length (see its construction).
    let entries_len = entries.len();
    let playlist = Arc::new(AsyncRwLock::new(Playlist { entries, index }));
    let roots = Arc::new(AsyncRwLock::new(roots));
    let (preset_count_tx, preset_count_rx) =
        tokio::sync::watch::channel(playlist.read().await.preset_count());

    // Two flags shared between the two halves: the page modifies the playlist,
    // the Source plays it, and nothing else links them — the SDK's
    // notifications deliberately carry no action.
    let playlist_changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let plays = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Created here, without probing anything: the circuit breaker only learns
    // by serving requests. Probing at startup would put a disk access back
    // before the socket is bound, which is precisely the defect it fixes.
    //
    // Shared with the Source: the cover lookup does a `read_dir` on the same
    // share as the Admin half, and must fall under the same circuit breaker
    // rather than inventing a second one.
    let health = Arc::new(ritornello_plugin_files::health::Health::new());

    let source = FilesSource {
        playlist: playlist.clone(),
        playlist_changed: playlist_changed.clone(),
        plays: plays.clone(),
        // Identity permutation matching what `playlist` was just built with:
        // shuffle is off until the core's first `SetPlayMode`, so position and
        // entry coincide from the start.
        order: (0..entries_len).collect(),
        draw: Order::Random,
        random: false,
        repeat_all: false,
        pass_finished: false,
        mode_changed: false,
        state_path: state_path.clone(),
        mpv_playlist_path,
        catalog: catalog.clone(),
        locales_root: locales_root.clone(),
        preset_count_rx: Some(preset_count_rx),
        cover_in_flight: None,
        cover_by_dir: Arc::new(Mutex::new(None)),
        health: health.clone(),
        // Cloned, not moved: `admin` below takes the table itself, and the
        // Source only ever reads it (see the field's doc).
        roots: roots.clone(),
        current_file: None,
    };

    // Probed at startup rather than on use: the page must be able to grey out
    // the network wizard as soon as it opens, like the System tab greys out
    // the reboot on `can_reboot`. The probe is redone at every connection
    // attempt, so that installing the package without rebooting gives a
    // correct result.
    let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(
        ritornello_plugin_files::smb::available().await,
    ));
    if !smb_ok.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("smbclient is not available: the network wizard will be offered read-only");
    }

    let admin = admin::FilesAdmin {
        explore: ritornello_plugin_files::explore::Browser::new(
            runtime_dir.clone(),
            catalog.clone(),
            smb_ok.clone(),
            health.clone(),
        ),
        health,
        mount_error: Arc::new(Mutex::new(None)),
        smb_ok,
        playlist_changed,
        plays,
        durations: Arc::new(Mutex::new(admin::DurationsProgress::default())),
        durations_task: None,
        roots_path,
        creds_dir,
        internal_playlists: playlists_dir,
        state_path,
        roots,
        playlist,
        catalog,
        locales_root,
        scan: Arc::new(Mutex::new(admin::ScanProgress::default())),
        scan_task: None,
        unresolved: Arc::new(Mutex::new(Vec::new())),
        browse: Arc::new(Mutex::new(serde_json::json!({}))),
        preset_count_tx,
    };
    Runtime::from_args(env!("CARGO_PKG_VERSION"))?.source(source)?.admin(admin)?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_plugin_files::roots::Root;
    use ritornello_proto::IdentityUpdate;

    /// An SMB root on `share`, with every field a `Root` must carry filled in
    /// with an otherwise-inert value — callers override only what their test
    /// cares about with `..smb_root(share)`.
    ///
    /// **Deliberately no `subpath`**: the sibling helper in `roots.rs` sets one
    /// ("Albums"), which is exactly wrong here — these tests build a file path
    /// under the root's plain mount point (`/mnt/ritornello/<name>/…`), and a
    /// subpath would make `base_dir()` a component longer than that, so
    /// `root_of` would no longer see the file as owned by this root.
    fn smb_root(share: &str) -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: share.into(),
            subpath: None,
            user: "steven".into(),
            domain: String::new(),
            writable: false,
            archive_covers: false,
        }
    }

    #[test]
    fn an_offer_is_made_only_for_a_probed_folder_without_an_image() {
        let dir = std::path::Path::new("/mnt/ritornello/nas/Album");
        let file = dir.join("01.flac");
        let table = Roots {
            root: vec![Root {
                name: "nas".into(),
                archive_covers: true,
                writable: true,
                ..smb_root("musique")
            }],
        };

        // Probed, nothing found: the one case that deserves an offer.
        assert!(offers_archive(&table, &file, FolderState::NoImage));

        // A cover was found: there is nothing to archive.
        assert!(!offers_archive(&table, &file, FolderState::Image));

        // **Not probed at all** — a share that did not answer in time stores
        // nothing. "Unknown" must never read as "no cover": offering here
        // would archive over an image nobody has looked for. Three named
        // states rather than two nested `Option`s, precisely because this is
        // the distinction this whole feature turns on.
        assert!(!offers_archive(&table, &file, FolderState::NotProbed));
    }

    #[test]
    fn an_offer_needs_the_flag_and_a_writable_share() {
        let file = std::path::Path::new("/mnt/ritornello/nas/Album/01.flac");
        let base = Root { name: "nas".into(), ..smb_root("musique") };

        // The flag off: no offer, whatever the mount says.
        let off = Roots { root: vec![Root { writable: true, ..base.clone() }] };
        assert!(!offers_archive(&off, file, FolderState::NoImage));

        // The flag on but the share read-only: no offer. The control is
        // greyed in the page, but a table edited by hand must not slip past.
        let ro = Roots { root: vec![Root { archive_covers: true, ..base.clone() }] };
        assert!(!offers_archive(&ro, file, FolderState::NoImage));

        // A **local** root has no read-only mount to speak of: the flag alone
        // decides, and the filesystem has the last word at write time.
        let local = Roots {
            root: vec![Root {
                name: "usb".into(),
                kind: RootKind::Local,
                path: Some("/media/usb".into()),
                archive_covers: true,
                writable: false,
                ..base
            }],
        };
        let on_usb = std::path::Path::new("/media/usb/Album/01.flac");
        assert!(offers_archive(&local, on_usb, FolderState::NoImage));
    }

    #[test]
    fn a_path_owned_by_no_root_is_never_offered() {
        let table = Roots { root: vec![] };
        let file = std::path::Path::new("/tmp/x/01.flac");
        assert!(!offers_archive(&table, file, FolderState::NoImage));
    }

    /// Builds a test `Metadata` for a (target, level) pair.
    ///
    /// `tracing::Metadata::new` requires `&'static str`: the tested targets
    /// are therefore literals, which is enough — the rule only bears on a
    /// prefix known in advance.
    fn frame(target: &'static str, level: tracing::Level) -> tracing::Metadata<'static> {
        tracing::Metadata::new(
            "frame",
            target,
            level,
            None,
            None,
            None,
            tracing::field::FieldSet::new(&[], tracing::callsite::Identifier(&CALLSITE)),
            tracing::metadata::Kind::EVENT,
        )
    }

    /// A dummy callsite, required by `FieldSet::new`. It is never registered
    /// nor consulted: only its identity serves as a key.
    struct Callsite;
    impl tracing::callsite::Callsite for Callsite {
        fn set_interest(&self, _: tracing::subscriber::Interest) {}
        fn metadata(&self) -> &tracing::Metadata<'_> {
            unreachable!("this callsite is never consulted")
        }
    }
    static CALLSITE: Callsite = Callsite;

    #[test]
    fn lofty_chatter_is_kept_out_of_the_log_but_not_its_errors() {
        // The reported symptom: "MPEG: Using bitrate to estimate duration", a
        // WARN per MP3 without a Xing header, which pushes real errors out of
        // the core's "last errors" buffer.
        assert!(!frame_to_log(&frame("lofty::mpeg::properties", tracing::Level::WARN)));
        assert!(!frame_to_log(&frame("lofty", tracing::Level::INFO)));
        // What the rule must above all not take away:
        assert!(frame_to_log(&frame("lofty::mpeg", tracing::Level::ERROR)));
        assert!(frame_to_log(&frame("ritornello_plugin_files", tracing::Level::WARN)));
        // And no matching by mere substring: a target that starts with the
        // same phrase without being `lofty` stays logged.
        assert!(frame_to_log(&frame("my_crate::lofty_helper", tracing::Level::WARN)));
    }

    /// A Source with no shuffle involved: `test_source(p)` used to be the
    /// whole builder, and stays the shorthand for every test that does not
    /// care about the drawn order.
    fn test_source(playlist: Playlist) -> FilesSource {
        source_with(playlist, Order::Sequential)
    }

    /// A Source whose draws come from `draw` rather than a real shuffle —
    /// what makes a pass reproducible enough to assert on in a test (see
    /// `Order`'s doc: a check against a real RNG would only ever be
    /// statistical).
    fn source_with(playlist: Playlist, draw: Order) -> FilesSource {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // The tempdir is deliberately leaked: the Source lives for the duration
        // of the test, and dropping it would erase the paths it writes.
        std::mem::forget(dir);
        let order = (0..playlist.entries.len()).collect();
        FilesSource {
            playlist: Arc::new(AsyncRwLock::new(playlist)),
            playlist_changed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            plays: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            order,
            draw,
            random: false,
            repeat_all: false,
            pass_finished: false,
            mode_changed: false,
            state_path: root.join("plugin-files.json"),
            mpv_playlist_path: root.join("plugin-files.m3u"),
            catalog: Arc::new(RwLock::new(Catalog::load("files", "en", &root, FILES_EN))),
            locales_root: root,
            preset_count_rx: None,
            cover_in_flight: None,
            cover_by_dir: Arc::new(Mutex::new(None)),
            health: Arc::new(ritornello_plugin_files::health::Health::new()),
            roots: Arc::new(AsyncRwLock::new(Roots::default())),
            current_file: None,
        }
    }

    /// Reads the entry currently designated by the playlist's index,
    /// synchronously: every test reaches this between two `.await` points, at
    /// which point nothing else holds the lock, so `try_read` never blocks.
    impl FilesSource {
        fn current_entry(&self) -> usize {
            self.playlist.try_read().expect("lock held only across .await points").index
        }
    }

    /// Entry played at the start of a `Play` outcome, read through the
    /// 1-based `preset` the outcome already carries — the same number the
    /// screen and the grid show.
    fn started_entry(out: &SourceOutcome) -> usize {
        usize::from(out.preset.expect("a Play outcome carries a preset")) - 1
    }

    /// Entry that sits at `position` in the drawn order — without driving
    /// playback there, since the point of `a_drawn_pass_plays_every_track_once`
    /// is to check the whole order was covered, not to replay it.
    fn entry_at_position(s: &FilesSource, position: usize) -> usize {
        s.entry_at(position).expect("a full pass covers every position")
    }

    fn playlist_of(n: usize) -> Playlist {
        Playlist {
            entries: (1..=n)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/musique/{i:02}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect(),
            index: 0,
        }
    }

    #[tokio::test]
    async fn a_drawn_pass_plays_every_track_once() {
        // The whole promise: not "a random pick at each next", but a pass that
        // covers everything without repeating. The draw is injected, so this is
        // exact and not statistical.
        let mut s = source_with(playlist_of(5), Order::Fixed(vec![3, 0, 4, 1, 2]));
        s.set_play_mode(true, false).await;
        let mut seen = vec![];
        let out = s.activate().await;
        seen.push(started_entry(&out));
        for position in 1..5 {
            seen.push(entry_at_position(&s, position));
        }
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3, 4], "each track exactly once");
    }

    #[tokio::test]
    async fn a_position_reported_by_mpv_is_read_through_the_order() {
        // The trap: `player_track(n)` used to take the mpv position for an
        // entry index. With a drawn order they are two different spaces, and
        // the screen would name the wrong track.
        let mut s = source_with(playlist_of(3), Order::Fixed(vec![2, 0, 1]));
        s.set_play_mode(true, false).await;
        s.activate().await;
        s.player_track(1).await; // second position of the drawn order
        assert_eq!(s.current_entry(), 0, "position 1 holds entry 0");
    }

    #[tokio::test]
    async fn next_after_toggling_shuffle_follows_the_drawn_order() {
        // `reload_if_changed(1)` walked `index + 1` in entry space: with this
        // order that would have landed on entry 0 (preset 1) once shuffle was
        // engaged. Reading the step through the drawn order instead lands on
        // entry 1 (preset 2).
        let mut s = source_with(playlist_of(3), Order::Fixed(vec![2, 1, 0]));
        s.set_play_mode(true, false).await;
        s.activate().await;
        // Simulate a playlist edit from the admin page: the one channel that
        // makes `next()` reload instead of delegating to mpv.
        s.playlist_changed.store(true, std::sync::atomic::Ordering::Relaxed);
        let out = s.next().await;
        assert!(matches!(out.action, SourceAction::Play { .. }), "{:?}", out.action);
        assert_eq!(out.preset, Some(2), "position-space next, not entry-space +1");
    }

    #[tokio::test]
    async fn activating_an_empty_playlist_launches_nothing_and_says_so() {
        let mut s = test_source(Playlist::default());
        let out = s.activate().await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert_eq!(out.preset_count, Some(0));
        assert!(out.status.is_some(), "the status must say why nothing is playing");
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn activating_resumes_at_the_remembered_track() {
        // Resuming after a reboot: without `start`, playback would restart at
        // the first track every time the device boots.
        let mut p = playlist_of(5);
        p.index = 3;
        let mut s = test_source(p);
        let out = s.activate().await;
        match out.action {
            SourceAction::Play { start, finite, .. } => {
                assert_eq!(start, Some(3));
                assert!(finite, "a list of files has a normal end");
            }
            other => panic!("expected a Play, got {other:?}"),
        }
        assert_eq!(out.preset, Some(4));
        assert_eq!(out.preset_count, Some(5));
        assert!(out.preset_name.is_some(), "the screen must never be blank");
    }

    #[tokio::test]
    async fn a_nonexistent_track_gives_a_transient_message_without_cutting_playback() {
        // Same rule as the radio's empty preset: nothing was launched, so the
        // previous track is still playing and must reappear on screen. Above
        // all: no identity declaration, otherwise the metadata of the current
        // track would be erased.
        let mut s = test_source(playlist_of(3));
        let out = s.select(9).await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert!(out.transient, "the message must fade by itself");
        assert!(out.identity.is_none(), "declaring a stop would be wrong");
        assert_eq!(out.preset_count, Some(3));
    }

    #[tokio::test]
    async fn the_status_is_redeclared_on_every_frame() {
        // TRAP: `status` has the OPPOSITE convention to `preset`. Absent means
        // "no status", and not "keep the previous one": a Source that omitted
        // it would see its display erase itself.
        let mut s = test_source(playlist_of(3));
        for (name, out) in [
            ("activate", s.activate().await),
            ("select", s.select(2).await),
            ("next", s.next().await),
            ("prev", s.prev().await),
            ("stop", s.stop().await),
        ] {
            assert!(out.status.is_some(), "status omitted on {name}: the screen would go blank");
        }
    }

    #[tokio::test]
    async fn automatic_advance_resyncs_index_identity_and_name() {
        // Real path: mpv moves to the next track by itself, the core relays
        // `PlayerTrack(n)`, and only the Source knows what "track n" means.
        let mut s = test_source(playlist_of(5));
        let out = s.player_track(2).await;
        assert_eq!(out.preset, Some(3));
        assert!(out.preset_name.is_some());
        assert_eq!(
            out.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "file", "path": "/musique/03.mp3"
            })))
        );
    }

    #[tokio::test]
    async fn a_negative_index_is_discarded_without_declaring_anything() {
        // mpv says -1 at the end of the list. The core passes it on as is; the
        // Source discards it, and above all declares nothing — the stop will
        // come from `stop()`.
        let mut s = test_source(playlist_of(3));
        let out = s.player_track(-1).await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert!(out.identity.is_none());
    }

    #[tokio::test]
    async fn the_end_of_the_playlist_declares_that_nothing_plays_any_more() {
        let mut s = test_source(playlist_of(3));
        let out = s.stop().await;
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn next_and_prev_delegate_to_mpv_without_resyncing_twice() {
        // Resyncing here on top of `player_track` would make two corrections
        // for a single change, and the second could contradict the first.
        let mut s = test_source(playlist_of(3));
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
        assert_eq!(s.prev().await.action, SourceAction::PlayerPrev);
        assert_eq!(s.playlist.read().await.index, 0, "the index must not have moved by itself");
    }

    #[tokio::test]
    async fn selecting_persists_the_track() {
        let mut s = test_source(playlist_of(4));
        s.select(3).await;
        assert_eq!(state::load(&s.state_path).index, 2);
    }

    #[tokio::test]
    async fn the_admin_half_announces_the_count_without_disturbing_playback() {
        // Modifying the playlist from the page must update the web remote's
        // grid right away, without waiting for a track to be played.
        //
        // **And renumber what is playing**, which was missing: reordering the
        // playlist changes the position of the track being listened to, and
        // the display's counter stayed on the old one — the plugin page was
        // right, the player was not.
        //
        // What remains guaranteed, and it is the essential part: no identity,
        // no status, no action. The current track is neither interrupted nor
        // redeclared, only renumbered. The core does keep `preset`,
        // `preset_name` and `preset_count` when absent — but **not** `status`,
        // which it replaces, absence included (see the comment in
        // `poll_notification`): it is its early return, for a frame that
        // declares neither identity nor status, that makes this notice
        // harmless.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut s = test_source(playlist_of(3));
        s.preset_count_rx = Some(rx);
        s.select(2).await;
        tx.send(7).unwrap();
        let n = s.poll_notification().await.expect("a notification expected");
        assert_eq!(n.preset_count, Some(7));
        assert_eq!(n.preset, Some(2), "the number must follow the track being listened to");
        assert!(n.preset_name.is_some(), "and the name with it");
        assert!(n.identity.is_none(), "what is playing must not be redeclared");
        assert!(n.status.is_none(), "nor the status touched");
        // The names travel with the count: without them, the grid would keep
        // the previous titles under the new numbers — worse than no title.
        assert_eq!(
            n.presets.as_deref().map(|p| p.len()),
            Some(3),
            "the named presets must accompany the count"
        );
    }

    #[tokio::test]
    async fn the_source_enumerates_its_named_presets() {
        // Without this override, the default body of `list_presets` returns an
        // empty list and the grid tiles only have a number — the defect
        // reported by the owner. The sources_catalog distinguishes "I only
        // have numbers" (empty list) from "here are my names", and this source
        // knows how to name.
        let mut s = test_source(playlist_of(2));
        let presets = s.list_presets().await;
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].index, 1);
        assert!(!presets[0].name.is_empty(), "every tile must carry a title");
    }

    #[tokio::test]
    async fn the_cover_placed_alongside_is_announced_after_a_play() {
        // The nominal case: a ripped CD puts its `cover.jpg` next to the
        // tracks. The lookup must happen after the `Play`, in the spontaneous
        // notification — not in the answer to `activate()`, which never
        // declares a cover (see `serve_source`).
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("01 - piste.flac");
        std::fs::write(&track, b"x").unwrap();
        std::fs::write(dir.path().join("cover.jpg"), b"x").unwrap();
        let mut s = test_source(Playlist {
            entries: vec![Entry { path: track, title: None, duration_s: None }],
            index: 0,
        });
        let out = s.activate().await;
        assert!(out.identity.is_some(), "the track must be declared as such");
        let n = s.poll_notification().await.expect("a notification expected");
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path {
                path: dir.path().join("cover.jpg").to_string_lossy().into_owned()
            })
        );
    }

    #[tokio::test]
    async fn automatic_advance_reannounces_the_cover_without_reprobing() {
        // The flagship use case of this whole layer, and it was wrong: on a
        // ripped album, only the track the user launches goes through
        // `play()`. The following ones arrive through `player_track`, which
        // answers with `resync()` — a **new identity**, hence a `cover_source`
        // reset on the core side (see `Metadata::set_identity`) — without ever
        // re-arming the probe. Result: cover on track 1, ♫ fallback on tracks
        // 2..N.
        let dir = tempfile::tempdir().unwrap();
        let cover = dir.path().join("cover.jpg");
        std::fs::write(&cover, b"x").unwrap();
        let entries: Vec<Entry> = (1..=3)
            .map(|i| {
                let p = dir.path().join(format!("{i:02} - piste.flac"));
                std::fs::write(&p, b"x").unwrap();
                Entry { path: p, title: None, duration_s: None }
            })
            .collect();
        let mut s = test_source(Playlist { entries, index: 0 });
        let expected = Some(ritornello_proto::CoverRef::Path {
            path: cover.to_string_lossy().into_owned(),
        });

        s.activate().await;
        assert_eq!(s.poll_notification().await.unwrap().cover, expected, "track 1");

        // mpv advances by itself. The cover must go out again with the new
        // identity.
        let out = s.player_track(1).await;
        assert!(out.identity.is_some(), "a new identity is indeed declared");
        assert_eq!(s.poll_notification().await.unwrap().cover, expected, "track 2");

        // And **without paying the `readdir` again**: the directory has not
        // changed. The proof is made by removing the image from disk — a real
        // probe would find nothing any more, yet the remembered value is
        // re-announced. This is what avoids an SMB round trip at every track
        // change.
        std::fs::remove_file(&cover).unwrap();
        s.player_track(2).await;
        assert_eq!(
            s.poll_notification().await.unwrap().cover,
            expected,
            "track 3: the value must come from memory, not from a new probe"
        );
    }

    #[tokio::test]
    async fn changing_directory_reprobes() {
        // The counterpart of the test above: remembering is per directory, so
        // moving to a neighbouring album must indeed probe again — otherwise
        // the second album would show the first one's cover.
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("album-un");
        let second = dir.path().join("album-deux");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("cover.jpg"), b"x").unwrap();
        std::fs::write(second.join("folder.png"), b"x").unwrap();
        let entries = vec![
            Entry { path: first.join("01.flac"), title: None, duration_s: None },
            Entry { path: second.join("01.flac"), title: None, duration_s: None },
        ];
        let mut s = test_source(Playlist { entries, index: 0 });
        s.activate().await;
        assert_eq!(
            s.poll_notification().await.unwrap().cover,
            Some(ritornello_proto::CoverRef::Path {
                path: first.join("cover.jpg").to_string_lossy().into_owned()
            })
        );
        s.player_track(1).await;
        assert_eq!(
            s.poll_notification().await.unwrap().cover,
            Some(ritornello_proto::CoverRef::Path {
                path: second.join("folder.png").to_string_lossy().into_owned()
            }),
            "a different directory must be probed"
        );
    }

    #[tokio::test]
    async fn the_absence_of_a_cover_does_not_block_the_other_notifications() {
        // Defended by the review: `poll_notification` must never return `None`
        // (terminal for the SDK) nor an empty notification when there is
        // nothing next to the file. The proof: the unrelated preset count
        // mechanism keeps working right after.
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("01 - piste.flac");
        std::fs::write(&track, b"x").unwrap(); // no image alongside
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut s = test_source(Playlist {
            entries: vec![Entry { path: track, title: None, duration_s: None }],
            index: 0,
        });
        s.preset_count_rx = Some(rx);
        s.activate().await;
        tx.send(3).unwrap();
        let n = s.poll_notification().await.expect("a notification expected, not None");
        assert_eq!(n.preset_count, Some(3));
        assert!(n.cover.is_none(), "no cover alongside, nothing to announce");
    }

    #[tokio::test]
    async fn cancelling_then_repolling_does_not_launch_a_second_probe() {
        // Defended by the review: a direct call to `health.bounded(...).await`
        // from `poll_notification` would get cancelled by the SDK's `select!`
        // as soon as a request from the core arrives — a common event, not an
        // edge case — losing `health`'s bookkeeping and launching one more
        // probe on the same share at every round. The fix: the probe lives on
        // an independent task (`play()` launches it), and `poll_notification`
        // only waits for its result on a `oneshot::Receiver`, cancel-safe by
        // construction.
        //
        // Counting the probes actually launched would require instrumenting
        // `cover::search` or forcing a real `health` timeout — which falls
        // back on a timing assumption that this crate has just expelled from
        // its tests (see the history). Instead, this test directly checks the
        // property that makes the second probe impossible: the single receiver
        // placed here survives intact the cancellation of a first round of
        // `poll_notification`, and the second round reads its answer on that
        // same channel — without any code of `poll_notification` needing to
        // open another one (there is simply no `tokio::spawn` in that method).
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut s = test_source(Playlist::default());
        s.cover_in_flight = Some(rx);

        // First round: nothing has been sent yet, `poll_notification` must stay
        // pending. `yield_now()` bounds the observation to a single pass of the
        // scheduler — a deterministic property of the runtime, not a wall
        // clock, so no room for a flake here.
        tokio::select! {
            _ = s.poll_notification() => panic!("must not resolve before something is sent"),
            _ = tokio::task::yield_now() => {}
        }

        // The previous future has been dropped (the exact equivalent of the
        // cancellation by the SDK's `select!`). The field must have survived
        // intact, still connected to this single receiver: if
        // `poll_notification` had opened a second one, this `send` — the only
        // sender that exists in this test — would have nobody to convince on
        // the fictitious second channel, and the next assertion would fail by
        // returning `None` rather than the cover.
        tx.send(Some(ritornello_proto::CoverRef::Path { path: "/nas/Album/cover.jpg".into() }))
            .unwrap();
        let n = s.poll_notification().await.expect("a notification expected, not None");
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/nas/Album/cover.jpg".into() })
        );
    }

    #[tokio::test]
    async fn toggling_repeat_all_alone_leaves_a_drawn_order_and_its_index_untouched() {
        // The trap: the core rediffuses `SetPlayMode` to every source on
        // every mode change — *either* mode — and also at wake-up and
        // hot-plug, so `random` is unchanged on most calls here. Redrawing
        // and repositioning regardless (an earlier version) silently moved
        // `order` and the index without ever rewriting mpv's m3u or emitting
        // an action: mpv kept walking its own file at its own position while
        // `order` came to describe a different one.
        // `Order::Sequence` with **two distinct** permutations, deliberately:
        // `Fixed` hands back the same vector on every draw, so a version that
        // wrongly redraws on every call would still pass this test by
        // accident — the second draw would just repeat the first. Only two
        // different permutations can tell "redrawn again" from "left alone"
        // apart.
        let mut s = source_with(playlist_of(3), Order::Sequence(vec![vec![2, 0, 1], vec![1, 0, 2]]));
        s.set_play_mode(true, false).await; // engages shuffle: draws [2, 0, 1]
        assert_eq!(s.drawn_order(), &[2, 0, 1]);
        assert_eq!(s.current_entry(), 2, "the pass starts on the first drawn entry");
        // `random` stays `true`; only `repeat_all` moves.
        s.set_play_mode(true, true).await;
        assert_eq!(s.drawn_order(), &[2, 0, 1], "no random transition: no redraw");
        assert_eq!(s.current_entry(), 2, "no random transition: the index must not move");
    }

    #[tokio::test]
    async fn engaging_shuffle_mid_playback_reloads_mpv_on_the_next_advance() {
        // Regression #4 (whole-branch review): `set_play_mode` has no
        // action of its own to return, so without `mode_changed` a mode
        // toggled while a file was already playing under mpv's own m3u
        // never actually reached mpv at all — the very next natural
        // advance (here simulated through `player_track`, mpv still
        // walking the *old*, sequential file) would instead have been
        // translated through the *new* drawn order onto the *old* file: a
        // wrong title, identity and cover, sound staying sequential, until
        // some later reload caught up.
        let mut s = source_with(playlist_of(4), Order::Fixed(vec![2, 0, 3, 1]));
        s.activate().await; // sequential playback, entry 0
        assert_eq!(s.current_entry(), 0);

        s.set_play_mode(true, false).await; // shuffle engaged mid-playback

        let out = s.player_track(1).await;
        assert!(
            matches!(out.action, SourceAction::Play { .. }),
            "the mode change must reload mpv, got {:?}",
            out.action
        );
        // Regression #4b (whole-branch review): entry 0 (already playing)
        // sits at the head of the draw, so the reload lands on the pass's
        // real *second* entry (2) — neither skipping it (which a naive
        // "index := order[0], then step" would have done) nor replaying
        // the entry already heard.
        assert_eq!(s.current_entry(), 2);
    }

    #[tokio::test]
    async fn toggling_repeat_all_alone_without_shuffle_leaves_the_index_untouched() {
        let mut p = playlist_of(3);
        p.index = 2;
        let mut s = source_with(p, Order::Sequential);
        s.set_play_mode(false, false).await;
        s.set_play_mode(false, true).await;
        assert_eq!(s.current_entry(), 2, "repeat-all alone never moves the index");
    }

    #[tokio::test]
    async fn a_pass_without_repeat_stops_at_the_end() {
        let mut s = source_with(playlist_of(3), Order::Fixed(vec![2, 0, 1]));
        s.set_play_mode(true, false).await;
        let out = s.end_of_content().await;
        assert!(matches!(out.action, SourceAction::Noop), "nothing more to play");
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn repeat_all_opens_a_new_pass_with_a_new_draw() {
        let mut s = source_with(playlist_of(3), Order::Sequence(vec![vec![2, 0, 1], vec![1, 2, 0]]));
        s.set_play_mode(true, true).await;
        let out = s.end_of_content().await;
        assert!(matches!(out.action, SourceAction::Play { .. }), "{:?}", out.action);
        assert_eq!(s.drawn_order(), &[1, 2, 0], "the next pass is drawn again, not replayed");
    }

    #[tokio::test]
    async fn repeat_all_without_shuffle_restarts_at_the_first_entry() {
        let mut s = source_with(playlist_of(3), Order::Sequential);
        s.set_play_mode(false, true).await;
        let out = s.end_of_content().await;
        match out.action {
            SourceAction::Play { start, .. } => assert_eq!(start, Some(0)),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn play_after_a_finished_pass_draws_a_fresh_one() {
        // Otherwise the documented behaviour ("Play replays the last track"
        // after a list ran out) would give one track and then a stop: the tail
        // of an exhausted order holds only the entry that was already the
        // last one played.
        let mut s = source_with(playlist_of(3), Order::Sequence(vec![vec![2, 0, 1], vec![1, 2, 0]]));
        s.set_play_mode(true, false).await; // no repeat: a pass really ends
        s.activate().await; // opens the first pass, order = [2, 0, 1]
        s.end_of_content().await; // the pass ends without opening another one
        let out = s.activate().await; // Play key pressed again
        assert!(matches!(out.action, SourceAction::Play { .. }), "{:?}", out.action);
        assert_eq!(s.drawn_order(), &[1, 2, 0], "a fresh pass is drawn, not the exhausted one replayed");
    }

    #[tokio::test]
    async fn the_source_declares_a_finite_list() {
        assert!(source_with(playlist_of(1), Order::Sequential).has_finite_list());
    }

    #[tokio::test]
    async fn the_status_follows_the_catalog_after_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(dir.path().join("files/fr.toml"), "status_files = \"FICHIERS\"\n").unwrap();
        let mut s = test_source(playlist_of(2));
        s.locales_root = dir.path().to_path_buf();
        s.set_locale("fr".into()).await;
        assert_eq!(s.activate().await.status.as_deref(), Some("FICHIERS"));
    }

    #[tokio::test]
    async fn a_stop_on_an_empty_playlist_says_the_playlist_is_empty() {
        // Reported defect, and a petty one: `play()` did display "NO PLAYLIST",
        // but without a track mpv stays idle, so the core sent `stop()` at
        // once — and that frame overwrote the message with a generic status.
        // The user could not learn that their playlist was empty.
        let mut s = test_source(Playlist::default());
        assert_eq!(s.activate().await.status.as_deref(), Some("NO PLAYLIST"));
        assert_eq!(s.stop().await.status.as_deref(), Some("NO PLAYLIST"));
    }

    #[tokio::test]
    async fn a_stop_on_a_full_playlist_stays_an_ordinary_stop() {
        // Guard rail: "no playlist" must stay reserved for the case where
        // there is really nothing to play.
        let mut s = test_source(playlist_of(3));
        s.activate().await;
        assert_eq!(s.stop().await.status.as_deref(), Some("FILES"));
    }

    #[tokio::test]
    async fn a_stop_announces_the_armed_track_and_not_a_bare_status() {
        // Reported defect: the display ended up "lost" — the status twice,
        // without number or name — after a stop. It must say the real state:
        // nothing is playing, and here is what will restart.
        let mut s = test_source(playlist_of(3));
        s.select(2).await;
        let outcome = s.stop().await;
        assert_eq!(outcome.preset, Some(2), "the armed track stays designated");
        assert!(outcome.preset_name.is_some(), "and named");
        assert_eq!(outcome.preset_count, Some(3));
        // But nothing is playing: that is what `plays_nothing` declares, and
        // that is what makes the "now playing" block disappear from the
        // display.
        assert!(outcome.identity.is_some(), "the stop must be declared, not silenced");
    }

    #[tokio::test]
    async fn a_stop_on_an_empty_playlist_designates_no_track() {
        // Nothing to arm: announcing a number would designate a track that
        // does not exist.
        let mut s = test_source(Playlist::default());
        let outcome = s.stop().await;
        assert_eq!(outcome.status.as_deref(), Some("NO PLAYLIST"));
        assert!(outcome.preset.is_none());
        assert_eq!(outcome.preset_count, Some(0));
    }

    #[tokio::test]
    async fn a_negative_playlist_pos_does_not_move_the_index() {
        // mpv announces `-1` at the end of the list **and transiently at every
        // reload**, hence at every track change: this is measured. Drawing a
        // conclusion from it — "the playlist has ended, let's restart from the
        // beginning" — made every resume fall back on track 1.
        let mut s = test_source(playlist_of(4));
        s.select(3).await;
        s.player_track(-1).await;
        assert_eq!(s.activate().await.preset, Some(3), "the -1 must conclude nothing");
    }

    #[tokio::test]
    async fn a_playlist_is_declared_as_such_to_the_core() {
        // The central defect: without `playlist`, the core loaded the m3u
        // through `loadfile`, which mpv only expands afterwards — the starting
        // index arrived out of bounds and every selection replayed the first
        // track.
        let mut s = test_source(playlist_of(3));
        match s.select(2).await.action {
            SourceAction::Play { playlist, start, finite, .. } => {
                assert!(playlist, "an m3u must be loaded as a playlist");
                assert_eq!(start, Some(1), "track 2 = index 1");
                assert!(finite, "a list of files has a normal end");
            }
            other => panic!("expected a Play, received {other:?}"),
        }
    }

    #[tokio::test]
    async fn resuming_after_a_stop_returns_the_track_being_listened_to() {
        // The Play key after a Stop asks for `activate()` again. It must
        // return the track we were listening to — the index lives in the
        // plugin and no stop moves it — and not restart from the first one.
        let mut s = test_source(playlist_of(4));
        s.select(3).await;
        s.stop().await;
        assert_eq!(s.activate().await.preset, Some(3), "the track being listened to, not the first");
    }

    #[tokio::test]
    async fn next_delegates_to_mpv_when_the_playlist_has_not_moved() {
        // The ordinary case: mpv holds the same list as we do, it knows how to
        // advance by itself. Reloading here would cut the sound for nothing.
        let mut s = test_source(playlist_of(3));
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
    }

    #[tokio::test]
    async fn next_hands_back_the_up_to_date_playlist_when_the_page_modified_it() {
        // Reported design defect: mpv plays a **copy** of the playlist,
        // written at the last `Play`. A track added since was out of its
        // reach, a track removed came back. The Admin half having no way to
        // tell mpv, we seize the first explicit command to hand it the fresh
        // playlist — a moment when the user expects a change anyway.
        let mut s = test_source(playlist_of(4));
        s.select(2).await;
        s.playlist_changed.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = s.next().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }), "{:?}", outcome.action);
        assert_eq!(outcome.preset, Some(3), "the following track, in the new playlist");
        // The drift is closed: the next command delegates to mpv again.
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
    }

    #[tokio::test]
    async fn prev_wraps_around_rather_than_blocking_after_a_modification() {
        // Without wrapping, a modification that leaves the listener on the
        // first track would make "previous" inoperative, whereas mpv wraps
        // from one end of its own list to the other.
        let mut s = test_source(playlist_of(3));
        s.playlist_changed.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(s.prev().await.preset, Some(3), "we come back to the last track");
    }

    #[tokio::test]
    async fn the_automatic_track_change_hands_back_the_up_to_date_playlist() {
        // The defect reported in use: resyncing only at the next explicit
        // command was not enough. If you modify the playlist and simply let
        // the track end, mpv chained on in the old one — hence "playlist
        // modifications, nothing".
        //
        // The automatic change is on the contrary the best moment: mpv starts
        // a file anyway, nothing is interrupted.
        let mut s = test_source(playlist_of(4));
        s.select(2).await;
        s.playlist_changed.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = s.player_track(2).await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }), "{:?}", outcome.action);
        assert_eq!(outcome.preset, Some(3), "the following track, in the up-to-date playlist");
    }

    #[tokio::test]
    async fn a_track_change_without_modification_reloads_nothing() {
        // The ordinary case, and by far the most frequent: reloading here would
        // cut the sound at every track change.
        let mut s = test_source(playlist_of(4));
        s.select(1).await;
        let outcome = s.player_track(1).await;
        assert!(matches!(outcome.action, SourceAction::Noop), "{:?}", outcome.action);
        assert_eq!(outcome.preset, Some(2), "we only restate where mpv is");
    }

    #[tokio::test]
    async fn the_end_of_the_playlist_does_not_relaunch_a_modified_playlist() {
        // At `-1` the playlist has ended. Reloading there would relaunch
        // playback instead of letting it finish — a playlist that loops
        // without being asked to.
        let mut s = test_source(playlist_of(3));
        s.playlist_changed.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = s.player_track(-1).await;
        assert!(matches!(outcome.action, SourceAction::Noop), "{:?}", outcome.action);
    }

    #[tokio::test]
    async fn next_on_a_cleared_playlist_plays_nothing() {
        // Clearing during playback: the stop is requested by the page, but if a
        // command arrives anyway, it must not look for a nonexistent track.
        let mut s = test_source(Playlist::default());
        s.playlist_changed.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = s.next().await;
        assert_eq!(outcome.status.as_deref(), Some("NO PLAYLIST"));
        assert!(matches!(outcome.action, SourceAction::Noop));
    }

    #[test]
    fn embedded_en_files_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(FILES_EN).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_mount_already_silent_is_reaped_rather_than_left_for_the_watcher() {
        // `already_silent` and an elapsed bound both make `bounded` answer
        // `None`, and only one of the two owns the staged file: on this
        // branch nothing ran, so nothing will ever come back for it. Built
        // with `Health::for_test`'s `silent` list rather than an actual
        // sleeping mount, so this reaches the branch deterministically and
        // without a three-second wait.
        let mut s = test_source(playlist_of(1));
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("01.flac");
        s.health = Arc::new(ritornello_plugin_files::health::Health::for_test(
            std::time::Duration::from_millis(50),
            String::new(),
            vec![track.clone()],
        ));
        let staged = dir.path().join("staged.jpg");
        std::fs::write(&staged, b"not a real image").unwrap();
        let identity = serde_json::json!({"kind": "file", "path": track.to_string_lossy()});

        s.archive_cover(identity, staged.to_string_lossy().into_owned()).await;

        // `archive_cover` detaches its work: poll rather than assume it has
        // run by the time this line is reached. The already-silent branch
        // never spawns anything blocking, so this settles almost at once.
        for _ in 0..50 {
            if !staged.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !staged.exists(),
            "a mount already known silent must have its staged file reaped, \
             since nothing else ever will"
        );
    }
}
