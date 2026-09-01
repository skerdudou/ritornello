//! The core: the `Core<P>` struct, its construction, and `handle_source_update`,
//! the entry point of a Source frame that writes into every domain.
//!
//! One domain per child module, each carrying its own partial
//! `impl<P: Player> Core<P>`. A child module sees the private fields of the
//! struct defined by its parent: that is what makes this split free — no
//! accessor, no `pub` field. Adding a domain means adding a file.
//!
//! - `commands`: remote and UI control — playback/standby, volume, tens, seek, startup
//! - `deadlines`: overlays and deadlines that the `main.rs` loop must wake up for
//! - `player`: mpv events, restart with growing backoff, resume on wake
//! - `metadata`: identity, ICY, tags, enrichments, covers and extraction
//! - `position`: progress reported by mpv, anchor set by a plugin
//! - `publish`: player state and sources_catalog pushed to displays, SPA and plugins
//! - `settings`: audio output, language, theme, writing `state.json`
//! - `sources`: cycle order, switching, hotplug arrival and a plugin's death, `apply`
//! - `test_support`: fake player and sources, rigs shared by the tests

use crate::metadata::{Metadata, PlayerState};
use crate::player::mpv;
use crate::player::Player;
use crate::state::{self, PersistedState, StartupPower};
use crate::types::Event;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::SourceUpdate;
use ritornello_proto::{
    SourcesCatalog, Command, Enrichment, IdentityUpdate, InputMessage, NowPlaying, Overlay, Playback,
    Preset, SourceAction, SourceCatalog, SourceReq,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};

mod commands;
mod deadlines;
mod playback;
mod track_metadata;
mod position;
mod publish;
mod settings;
mod sources;
pub use deadlines::next_deadline;

#[cfg(test)]
mod test_support;

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Source: Send + Sync + 'static {
    async fn request(&self, req: SourceReq) -> Result<SourceAction>;
}

/// What the main loop must do with a player event.
///
/// It is the core that decides which variants attest the stream's liveness
/// (`StreamAlive`): the loop in `main`, which holds the restart deadline,
/// follows this verdict instead of duplicating the list of variants — the
/// two lists had already started needing to be kept in sync by hand.
#[derive(Debug, PartialEq, Eq)]
pub enum EventOutcome {
    /// Nothing to do timing-wise.
    Nothing,
    /// The stream is alive: cancel any scheduled restart.
    StreamAlive,
    /// Schedule a stream restart after this delay.
    RetryIn(Duration),
}

/// Everything the core receives from `main`'s wiring: its sources, its
/// persisted state, its output channels.
///
/// A named struct rather than a long list of positional parameters: at eight
/// elements, a call's argument order can no longer be checked by eye, and two
/// neighboring `PathBuf`s (`state_path`, `locales_root`) would swap without
/// the compiler objecting.
pub struct Wiring {
    pub sources: HashMap<String, Arc<dyn Source>>,
    pub persisted: PersistedState,
    pub state_path: PathBuf,
    pub catalog: Arc<RwLock<Catalog>>,
    pub locales_root: PathBuf,
    pub metadata: MetadataWiring,
    /// The sources_catalog of sources going to the Display plugins, on **its
    /// own** channel. Not in `MetadataWiring`: it goes to neither the SPA nor
    /// the `metadata` plugins, and above all not into `state` — a
    /// sources_catalog is structural and rarely changing, widening it would
    /// send the names of 51 stations on every single state frame per second
    /// of playback.
    pub sources_catalog: watch::Sender<SourcesCatalog>,
}

/// Metadata wiring.
pub struct MetadataWiring {
    /// Names of the `metadata` plugins, **in the declaration order** of
    /// `plugins.toml`: that order is the arbitration priority.
    pub plugins: Vec<String>,
    /// What is playing, sent to the `metadata` plugins. A `watch` and not a
    /// direct call: a plugin that no longer reads must not be able to stall
    /// the core.
    pub now_playing: watch::Sender<NowPlaying>,
    /// Player state, sent to the SPA (route `GET /api/player`) and to the
    /// Display plugins: a single structured state channel for both, each
    /// composing what it wants from the same frame.
    pub state: watch::Sender<PlayerState>,
}

pub struct Core<P: Player> {
    player: P,
    sources: HashMap<String, Arc<dyn Source>>,
    source_order: Vec<String>,
    active_source: String,
    volume: u8,
    muted: bool,
    standby: bool,
    /// Standby as `state.json` had it at launch — the only thing
    /// `StartupPower::Previous` needs, and the reason it is a snapshot and
    /// not a re-read: `startup` runs after `new`, and by then `persist`
    /// may already have rewritten the file.
    persisted_standby: bool,
    expecting_stream: bool,
    /// Something is playing back, **whatever its nature**.
    ///
    /// Distinct from `expecting_stream`, which now only says "what is
    /// playing is a live stream that might drop, so it needs restarting".
    /// The two coincided as long as only streams were involved; since a
    /// Source can declare finite content (`Play { finite: true }`),
    /// `expecting_stream` is false while a disc or a file playlist is
    /// playing. Using it as a "something is playing" guard would silence
    /// the whole metadata layer for exactly that content.
    playback: bool,
    /// The ongoing playback is **paused**. Only meaningful while `playback`
    /// is true; `player_state` does not consult it otherwise.
    ///
    /// Reset to false at **the single place** where `playback` becomes true.
    /// This is the same doctrine `player_state` already applies to
    /// `position_s`: a single point cannot be forgotten, whereas five
    /// separate resets could be missed at the sixth path added.
    paused: bool,
    retry_count: u32,
    audio_device: Option<String>,
    /// Temporary overlay (volume/mute/message): the text to show plus its
    /// deadline. Carried by `PlayerState::overlay`, which the display plugin
    /// draws ahead of anything else.
    overlay: Option<(Overlay, Instant)>,
    /// Numbered key matching what is playing, declared by the active Source
    /// (see `SourceMessage::preset`). Forgotten as soon as nothing is
    /// playing anymore — `set_identity(None)` is authoritative here, just
    /// like for the metadata slate.
    preset: Option<u8>,
    /// Readable name of the current preset, declared by the active Source
    /// (see `SourceMessage::preset_name`). Lives and dies with `preset`:
    /// `set_identity(None)` is authoritative for both, and nowhere else —
    /// standby, source change and stop all call `set_identity(None)`, so
    /// this single point already covers them.
    preset_name: Option<String>,
    /// Permanent status declared by the active Source, already translated
    /// (see `SourceMessage::status`). Replaced by every non-transient frame,
    /// including by its absence — see the convention test.
    source_status: Option<String>,
    /// Resolved standby label, memoized at construction and on every
    /// `set_locale` — never at the moment standby is entered: the
    /// sources_catalog is read behind an async lock, and `player_state` is
    /// not async. Resolving it when standby is entered required two
    /// fallible `await`s before reaching it (`Command::Power`): a Source or
    /// mpv being unreachable on the first entry into standby would publish
    /// `standby: true` with no status at all, and the screen went entirely
    /// black. Resolved ahead of time, the field is always fresh and this
    /// ordering trap disappears. Wins over `source_status` in
    /// `player_state` — the device is asleep, whatever the source was
    /// saying no longer applies.
    standby_status: Option<String>,
    /// How many numbered presets the active source offers (stations,
    /// tracks), as last declared. Forgotten on source change and standby —
    /// the next source re-declares it on activate/wake — but kept on stop:
    /// a stopped radio still has its stations.
    preset_count: Option<u8>,
    /// Whether the active source has anything to eject, as last declared
    /// (`SourceMessage::can_eject`). Forgotten with the same timing as
    /// `preset_count` — source change and standby — for the same reason: it
    /// describes the source that is gone. **False, not `None`**, when nobody
    /// declares: not knowing means offering nothing, so the web remote greys
    /// its Eject key rather than sending a command into the void.
    can_eject: bool,
    /// The named presets **of each source**, indexed by source name, as each
    /// declared them (`SourceMessage::presets`).
    ///
    /// Separate from `preset_count`, and this is not a redundancy:
    /// `preset_count` describes the **active** source and is forgotten with
    /// it, whereas a table indexed by name describes *all* sources at once.
    /// That is what an MPD client requires, asking `listplaylistinfo
    /// "radio"` while the cd is playing. So nothing forgets it: neither
    /// switching source, nor standby.
    presets_par_source: HashMap<String, Vec<Preset>>,
    /// Remote tens offset in flight: `Plus10` presses accumulate here until
    /// a digit key consumes them (`+10` then `4` selects 14). Cleared by the
    /// overlay's own deadline (`expire_overlay`) or by its consumption
    /// (`Select`), and just as much by `apply_command`'s abandon
    /// guard — which also clears `self.overlay` in that third case, so an
    /// abandoned offset never leaves its `+NN` behind on a display that no
    /// longer means it.
    pending_tens: u8,
    state_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    locale: Option<String>,
    locales_root: PathBuf,
    theme: Option<String>,
    mode: Option<String>,
    /// Track metadata: identity of what is playing, ICY title, and plugin
    /// enrichments. See `metadata.rs` for the arbitration.
    metadata: Metadata,
    now_playing_tx: watch::Sender<NowPlaying>,
    state_tx: watch::Sender<PlayerState>,
    /// The sources_catalog of sources going to the displays. A channel
    /// separate from `state_tx`, and never published by `publish_state`: see
    /// `publish_catalog`.
    sources_catalog_tx: watch::Sender<SourcesCatalog>,
    /// Behavior settings (hold-to-repeat timings, startup power state),
    /// persisted with the rest of the state.
    settings: crate::state::Settings,
    /// Hold-to-repeat pacing: instant before which a held volume command is
    /// ignored. Armed by a fresh volume step (now + initial delay), re-armed
    /// by each applied repeat (now + interval). `None` until a first press —
    /// a held event arriving out of nowhere (core restarted mid-hold) does
    /// nothing.
    volume_deadline: Option<Instant>,
    /// Where what is playing stands, in whole seconds, as the last refresh
    /// established it. Published as-is by `player_state`.
    position_s: Option<u32>,
    /// Duration **measured by mpv**, distinct from one a `metadata` plugin
    /// might announce. Kept separate because it supersedes it: merging them
    /// into a single field would lose track of who spoke, and precedence
    /// would become a write order — the kind of invariant that breaks
    /// silently.
    measured_duration_s: Option<u32>,
    /// Position announced by a `metadata` plugin, and the instant it
    /// arrived. The core advances it itself between two announcements —
    /// Radio France only polls the live feed every few dozen seconds, and
    /// without this advance the progress bar would stay frozen between two
    /// responses.
    position_anchor: Option<(u32, Instant)>,
    /// Cache shared with the router: the detached task deposits into it, the
    /// route reads it. **The same `Arc`** as the one handed to the HTTP
    /// `AppState` — see the note at its construction site in `main.rs` —
    /// without which a cover downloaded by the core would never be readable
    /// by the route.
    covers: Arc<crate::cover::CoverCache>,
    /// Results of detached retrievals, consumed by the `main` loop (see its
    /// `pochette_rx.recv()` branch). The boolean says whether the retrieval
    /// succeeded — needed so that `cover_arrived` releases `cover_in_flight`
    /// even on failure, instead of leaving that key stuck for the rest of
    /// the process.
    cover_tx: mpsc::Sender<(String, bool)>,
    /// Key whose retrieval is in flight, so it is not launched twice.
    cover_in_flight: Option<String>,
    /// Last path announced by mpv (`Event::Path`), kept **only** for
    /// comparison when a detached extraction arrives — never interpreted,
    /// per the principle laid down for `OBSERVED`. An extraction launched
    /// for a path can come back after mpv has moved on to another one:
    /// without this trace, its result would land after the fact on the next
    /// track.
    current_path: Option<String>,
    /// Path whose embedded extraction is currently in flight, so a second
    /// one is not launched while the first is still running on that same
    /// file.
    extraction_in_flight: Option<String>,
    /// Result of an extraction detached by `handle_path`, consumed by
    /// `main`'s `select!` loop (see `extraction_arrived`). Symmetric to
    /// `cover_tx` above.
    extraction_tx: mpsc::Sender<(String, Option<crate::cover::CoverSource>)>,
    /// Circuit breaker that bounds the `lofty` call, strictly blocking and
    /// potentially on a network share: see `health.rs` and the comment on
    /// `handle_path`.
    health: Arc<crate::health::Health>,
}

/// Resolves the standby label from a sources_catalog already in hand.
///
/// A free function rather than a method: it serves both construction (the
/// sources_catalog read via `try_read`, before `self` exists) and
/// `set_locale` (the sources_catalog just loaded, before it replaces the
/// core's), so neither needs to go through the async lock a second time.
fn resolve_standby_status(catalog: &Catalog) -> String {
    catalog.get("standby").to_string()
}

impl<P: Player> Core<P> {
    pub fn new(
        player: P,
        wiring: Wiring,
        covers: Arc<crate::cover::CoverCache>,
        cover_tx: mpsc::Sender<(String, bool)>,
        extraction_tx: mpsc::Sender<(String, Option<crate::cover::CoverSource>)>,
    ) -> Self {
        let Wiring { sources, persisted, state_path, catalog, locales_root, metadata, sources_catalog } =
            wiring;
        let mut source_order: Vec<String> = sources.keys().cloned().collect();
        source_order.sort();
        let active_source = if sources.contains_key(&persisted.active_source) {
            persisted.active_source.clone()
        } else {
            source_order.first().cloned().unwrap_or_default()
        };
        // Resolved right away: the only writer of this sources_catalog is
        // `set_locale`, reachable only from the `select!` loop that only
        // starts after this function returns — so no concurrent lock can
        // exist at this instant. See `resolve_standby_status` for the
        // reason behind this choice (never resolved again when standby is
        // entered).
        //
        // The failure is still logged rather than swallowed: it would leave
        // the standby screen entirely blank until the next language change
        // — precisely the defect this precomputation fixes. An invariant
        // believed to hold and that nobody checks is what produced that
        // defect the first time.
        let standby_status = match catalog.try_read() {
            Ok(cat) => Some(resolve_standby_status(&cat)),
            Err(_) => {
                tracing::warn!(
                    "standby label unavailable at startup: the standby screen will stay blank until the next locale change"
                );
                None
            }
        };
        let core = Self {
            player,
            sources,
            source_order,
            active_source,
            // Reclamped on playback: `state.json` can have been edited by
            // hand, and a `volume: 255` would go straight to mpv on wake.
            volume: persisted.volume.min(100),
            muted: false,
            standby: false,
            persisted_standby: persisted.standby,
            expecting_stream: false,
            playback: false,
            paused: false,
            retry_count: 0,
            audio_device: persisted.audio_device.clone(),
            overlay: None,
            preset: None,
            preset_name: None,
            source_status: None,
            standby_status,
            preset_count: None,
            can_eject: false,
            presets_par_source: HashMap::new(),
            pending_tens: 0,
            state_path,
            catalog,
            locale: persisted.locale.clone(),
            locales_root,
            theme: persisted.theme.clone(),
            mode: persisted.mode.clone(),
            metadata: Metadata::new(metadata.plugins),
            now_playing_tx: metadata.now_playing,
            state_tx: metadata.state,
            sources_catalog_tx: sources_catalog,
            settings: persisted.settings.clone(),
            volume_deadline: None,
            position_s: None,
            measured_duration_s: None,
            position_anchor: None,
            covers,
            cover_tx,
            cover_in_flight: None,
            current_path: None,
            extraction_in_flight: None,
            extraction_tx,
            health: Arc::new(crate::health::Health::new()),
        };
        // The sources wired at startup are already known: without this
        // publication, the channel would keep its blank
        // `SourcesCatalog::default()` and a display connecting before the
        // first preset would believe the device has no source. `add_source`
        // covers the rest.
        core.publish_catalog();
        // The persisted settings reach the cover cache here, and not only
        // on the first `set_settings`: without this line, a device whose
        // `state.json` disables re-encoding would only apply it starting
        // from the first visit to the config page, and would push full-size
        // images until then. Startup must obey the file.
        core.covers.set_cover_settings(crate::cover::CoverSettings::from(&core.settings));
        core
    }

    /// Applies what a Source reports: its status, and/or the identity of
    /// what it is now playing.
    ///
    /// Both arrive in the same frame and are applied together, with no
    /// intermediate display: no observable instant sees the displayed line
    /// describe one track and the identity announced to the plugins
    /// describe another.
    ///
    /// Two kinds of frames arrive on this channel, and they do not take the
    /// same path:
    ///
    /// - those that **recompose the view** — a Source response, declaring
    ///   an identity or a status, or a transient word to overlay;
    /// - those that **announce a fact** without saying anything about what
    ///   is playing — named presets, their count, the eject drawer,
    ///   renumbering the current track, the cover. These return early
    ///   before status handling: see the early return.
    ///
    /// **In practice, almost every production frame takes the second path
    /// as soon as it declares neither identity nor status**: the predicate
    /// that opens it is a tautology for the SDK (see the body). Every field
    /// must therefore be applied **on both paths** — whatever is only
    /// applied at the bottom of the function is never applied. The
    /// exhaustive destructuring at the top of the function is what makes
    /// this rule mandatory for every field added later.
    pub fn handle_source_update(&mut self, name: &str, update: SourceUpdate) {
        // **A frame from a source the core no longer knows is dropped, and
        // entirely so.** The fan-out of sources_catalog requests is
        // detached: a `ListPresets` runs in its own task, and
        // `remove_source` can run between the request and the response — a
        // plugin turned off from the UI, or dead on its own. Without this
        // guard, the still-in-flight response would re-insert the list into
        // `presets_par_source` **after** the eviction, because this
        // insertion is deliberately done before the active-source guard
        // (the sources_catalog describes every source, not the one that is
        // playing). The republished sources_catalog would then announce a
        // list registered for a source that no longer exists, an MPD client
        // would cache it, and a `load` on it was only refused at the last
        // moment by `Command::SelectSource`'s guard — i.e. after having
        // lied to the user.
        //
        // `sources` and not `source_order`: both are removed together by
        // `remove_source`, but `sources` is the table that says what the
        // core can still reach. The guard cannot refuse a legitimately
        // early frame: at startup, clients are wired before the loop drains
        // the channel, and hotplug wiring is *awaited* from the main loop,
        // which therefore processes no frame during that time.
        if !self.sources.contains_key(name) {
            tracing::debug!("source update for {name} dropped: no longer a wired source");
            return;
        }
        // **Exhaustive destructuring, and it is this function's main
        // safeguard.** No `..`: adding a field to `SourceUpdate` no longer
        // compiles until someone decides, here, which of the two halves it
        // belongs to — the `carries_a_fact` predicate **and** its
        // application on both paths.
        //
        // Derived rather than requested, and this is a lesson that was
        // paid for. The cover-art project's merge added `cover` to the
        // predicate without applying it on the early-return path: the
        // field was guarded, so the frame went through, but its
        // application lived at the very bottom of the function, after a
        // `return` that this frame always took. Every Source cover was
        // lost **silently**. Nothing flagged it: the predicate tests
        // fields one by one, and `SourceUpdate` derives `Default`, so a
        // tenth field breaks no literal and no test. A comment demanding
        // "think about both halves" is read after the fact; an exhaustive
        // destructuring cannot be forgotten. Same principle as a plugin's
        // announcement, which cannot lie about its kinds because they are
        // inferred, not declared.
        let SourceUpdate {
            identity,
            transient,
            preset,
            preset_count,
            preset_name,
            status,
            can_eject,
            presets,
            cover,
        } = update;
        // Read **before** the guard below, and this is intentional: the
        // sources_catalog describes every source, not the one that is
        // playing. An MPD client polls `listplaylistinfo "radio"` while the
        // cd is playing, and standby changes nothing about what a source
        // contains. The guard, on the other hand, protects what describes
        // **what is playing** — identity, status, transient message — and
        // stays in place for everything else.
        let carries_presets = presets.is_some();
        if let Some(presets) = presets {
            self.presets_par_source.insert(name.to_string(), presets);
            self.publish_catalog();
        }
        if self.standby || name != self.active_source {
            return;
        }
        // `preset_count` and `can_eject` describe the **active source** —
        // how many presets it offers, whether it has something to eject —
        // and their field docs already name them as a pair, forgotten
        // together on source switch and standby. Applied here, **before**
        // the early return, so the latter cannot swallow them; the order
        // relative to identity has no effect, `set_identity` does not touch
        // them.
        if let Some(c) = preset_count {
            self.preset_count = Some(c);
        }
        if let Some(e) = can_eject {
            self.can_eject = e;
        }
        // **The two paths, and which of the two actually carries the
        // safety.**
        //
        // The status handling, just below, *replaces* the remembered
        // status with whatever the frame carries, absence included: a
        // silent permanent frame **clears** whatever the source had
        // declared. The early return exists so that frames that only
        // announce a fact never reach it.
        //
        // **`carries_a_fact` is, in practice, a tautology, and this must be
        // known.** `serve_source` stamps `can_eject:
        // Some(plugin.can_eject())` on **each** of the two frames it
        // writes — the correlated response and the spontaneous
        // notification — and `SourceClient` copies it through unchanged
        // (see the doc of `SourceMessage::can_eject`: "The SDK stamps it
        // on **every** frame"). Every frame coming from the SDK therefore
        // arms `can_eject.is_some()`, hence arms `carries_a_fact`. The
        // other clauses change nothing for a production frame: they are an
        // **insurance** in case the stamping ever becomes conditional, not
        // a live guard.
        //
        // The consequence that matters is this: it is **not** the
        // predicate that protects anything, it is applying each field **on
        // both paths**. A frame that only announces a fact takes the early
        // return regardless; whatever is only applied at the bottom of the
        // function is therefore never applied at all. That is exactly the
        // defect the cover-art project's merge produced — `cover` guarded
        // but applied only at the bottom, so every cover lost silently —
        // and it is the destructuring at the top of the function, not this
        // comment, that prevents its recurrence.
        //
        // The historical case of the erased status, though, can no longer
        // come from the SDK: `preset_count` alone (the `plugin-files`
        // admin page saving a list) used to blank out "NO DISC" on the
        // console and the SPA until the next command, and it is the early
        // return that fixed it.
        //
        // `recomposes_the_view` mirrors the SDK's invariant word for word:
        // only a declared identity or status attest a view recomposition,
        // and `transient` joins them because a transient word is a
        // statement about what is playing (it must keep its overlay and
        // disarm a `+NN` in flight). `preset`, `preset_name`,
        // `preset_count`, `can_eject`, `presets` and `cover` attest
        // nothing: all of them follow the "absent = keep" convention, so
        // none can prove the frame describes the whole view.
        let recomposes_the_view = transient || identity.is_some() || status.is_some();
        let carries_a_fact = carries_presets
            || preset_count.is_some()
            || can_eject.is_some()
            || preset.is_some()
            || preset_name.is_some()
            || cover.is_some();
        if carries_a_fact && !recomposes_the_view {
            // A **single** call, and that is the point: the "absent =
            // keep" fields that must be applied after identity all live in
            // `apply_declared_facts`, called here and at exactly one other
            // place at the bottom of the function. A field added there
            // therefore lands on both paths by construction, instead of
            // depending on someone remembering to copy it — that is
            // exactly the oversight that made every Source cover get lost
            // silently.
            self.apply_declared_facts(preset, preset_name, cover, name);
            // Publish anyway: count, drawer and selection are part of the
            // broadcast state, and the channel dedupes if nothing changed.
            self.publish_state();
            return;
        }
        // `status` is reasserted by every permanent frame: absent means
        // cleared — the convention is the **opposite** of `preset`'s, and
        // the only one that allows clearing a status ("NO DISC" must be
        // able to disappear once a disc is inserted). A transient frame,
        // on the other hand, does not touch the remembered status: its
        // word goes into the overlay below, not here.
        if !transient {
            self.source_status = status.clone();
        }
        if transient {
            // Transient message ("empty preset"): it borrows the slot and
            // deadline of the volume/mute overlay, so `self.source_status`
            // — the permanent status — is kept and reappears on its own.
            // Without this, the message would stay on screen indefinitely
            // while playback continued on the previous station: the
            // display would durably describe a state that no longer
            // existed. `overlay_ms`, not `tens_window_ms`: this message has
            // nothing to do with the remote's `+NN` offset, only the
            // volume/mute overlay shares its deadline with it.
            //
            // An ongoing `+NN` offset therefore loses its display slot
            // here: disarming it along with it is what keeps it from
            // surviving behind a screen that no longer shows it (same
            // reason as `apply_command`'s abandon guard) — whether or not
            // the frame carries a word to display.
            self.pending_tens = 0;
            if let Some(message) = status {
                let deadline = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
                self.overlay = Some((
                    Overlay::Message { text: message, remaining_ms: self.settings.overlay_ms },
                    deadline,
                ));
            }
        }
        if let Some(identity) = identity {
            let value = match identity {
                IdentityUpdate::Playing(v) => Some(v),
                IdentityUpdate::Nothing => None,
            };
            self.set_identity(value);
        }
        // The second — and last — caller of `apply_declared_facts`, here
        // **after** identity: `set_identity(None)` clears the selection,
        // and `set_identity` by itself resets everything `Metadata` was
        // holding, including the Source's cover. A frame that explicitly
        // declares either one must win over this reset, so it is applied
        // after it. It is this ordering that forbids moving this call up
        // alongside `preset_count`; the early-return path, meanwhile,
        // cannot carry an identity by construction, so calling it there is
        // safe.
        self.apply_declared_facts(preset, preset_name, cover, name);
        // `preset_count` and `can_eject` are applied **at the top** of this
        // function, before the early return, for the same reason.
        //
        // Always publish: the current selection is part of the broadcast
        // state, and this call covers the frame that changes neither
        // identity nor metadata (the other paths already publish, and the
        // channel dedupes). `player_state` always carries the active
        // overlay alongside everything else: a source frame arriving
        // during an overlay therefore updates
        // source_status/preset/preset_name without changing anything the
        // display shows while it lasts.
        self.publish_state();
    }

    /// Everything a Source frame declares that must be applied **after**
    /// identity, in a single place.
    ///
    /// **It is the placement, not a comment, that makes the rule
    /// enforceable.** `handle_source_update` has two exits — the early
    /// return for frames that only announce a fact, and the bottom of the
    /// function for those that recompose the view — and a field applied at
    /// only one of the two places gets silently lost at the other. It
    /// happened twice: `presets`, then `cover`, the latter guarded by the
    /// predicate but applied only at the bottom, hence never, since a
    /// Source cover always arrives alone and always takes the early
    /// return. The exhaustive destructuring at the top of the function
    /// forces the *question* ("which of the two halves does this field
    /// belong to?") but not the *answer*: two side-by-side calls could
    /// still diverge. With a single body called at both exits, the answer
    /// is structural for every field added here.
    ///
    /// The limit, stated plainly: nothing stops someone from writing a new
    /// field *next to* this call rather than inside it one day. What is
    /// guaranteed is that no field already routed through here can be
    /// missing on either path.
    fn apply_declared_facts(
        &mut self,
        preset: Option<u8>,
        preset_name: Option<String>,
        cover: Option<ritornello_proto::CoverRef>,
        name: &str,
    ) {
        self.apply_selection(preset, preset_name);
        self.apply_source_cover(cover, name);
    }

}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[tokio::test]
    async fn standby_ignores_source_updates_and_wake_resumes_them() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(state_rx.borrow_and_update().standby);
        let mut update = bare_update();
        update.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update.clone());
        assert_eq!(state_rx.borrow().preset_name, None, "in standby, the source frame is ignored");
        core.handle_command(Command::Power).await.unwrap();
        core.handle_source_update("radio", update);
        assert_eq!(
            state_rx.borrow_and_update().preset_name.as_deref(),
            Some("FIP"),
            "waking up lets the source take over again"
        );
    }

    #[tokio::test]
    async fn update_from_inactive_source_is_ignored() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update_cd = bare_update();
        update_cd.preset_name = Some("CD".into());
        core.handle_source_update("cd", update_cd);
        assert_eq!(state_rx.borrow().preset_name, None, "the update from \"cd\" (inactive) was not applied");
        let mut update_radio = bare_update();
        update_radio.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update_radio);
        assert_eq!(state_rx.borrow_and_update().preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn source_status_does_not_survive_entering_standby() {
        // Second I2 scenario: without an explicit clear, `source_status`
        // stayed in memory during standby (masked by the standby word's
        // priority in `player_state`) and reappeared on wake as long as the
        // Source had not spoken again — a lie ready to resurface.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update = bare_update();
        update.status = Some("no disc".into());
        core.handle_source_update("radio", update);
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("no disc"));
        core.handle_command(Command::Power).await.unwrap(); // standby
        core.handle_command(Command::Power).await.unwrap(); // wake, silent source
        assert_eq!(
            state_rx.borrow_and_update().status,
            None,
            "the old frame's status must not reappear on wake before the Source has spoken again"
        );
    }

    #[tokio::test]
    async fn preset_count_is_remembered_and_published() {
        // A frame declaring a count must end up in PlayerState; a frame
        // silent on the subject must not clear it.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_count(Some(23)));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_source_update("radio", update_with_count(None));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        // Some(0) overwrites: the cd with no disc says "nothing to number".
        core.handle_source_update("radio", update_with_count(Some(0)));
        assert_eq!(state_rx.borrow().preset_count, Some(0));
    }

    #[tokio::test]
    async fn eject_capability_is_remembered_and_published() {
        // False by default: not knowing means offering nothing — the web
        // remote greys its Eject key until someone has claimed it. A frame
        // silent on the subject does not clear it.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        assert!(!state_rx.borrow().can_eject, "nothing declared: nothing offered");
        core.handle_source_update("radio", update_with_eject(Some(true)));
        assert!(state_rx.borrow().can_eject);
        core.handle_source_update("radio", update_with_eject(None));
        assert!(state_rx.borrow().can_eject, "a silent frame does not remove the capability");
        core.handle_source_update("radio", update_with_eject(Some(false)));
        assert!(!state_rx.borrow().can_eject);
    }

    #[tokio::test]
    async fn eject_survives_stop_but_neither_source_change_nor_standby() {
        // Same forgetting schedule as `preset_count`, and for the same
        // reason: the capability describes the Source, not what is
        // playing. Stopping does not change the fact the player has a
        // drawer; changing source does.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_eject(Some(true)));
        core.handle_command(Command::Stop).await.unwrap();
        assert!(state_rx.borrow().can_eject, "a drawer does not disappear on stop");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(!state_rx.borrow().can_eject, "the capability describes the source that is leaving");
    }

    #[tokio::test]
    async fn standby_removes_the_eject_capability() {
        // Standby lets no command through (`handle_command`): offering
        // Eject there would be one more lie. A fresh core per test — after
        // `SourceCycle`, nothing guarantees "radio" is still the active
        // source, so nothing guarantees a frame concerning it clears
        // `handle_source_update`'s guard.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_eject(Some(true)));
        assert!(state_rx.borrow().can_eject);
        core.handle_command(Command::Power).await.unwrap();
        assert!(!state_rx.borrow().can_eject);
    }

    #[tokio::test]
    async fn count_survives_stop_but_not_source_change() {
        // Stop clears preset (nothing is playing anymore) but not the
        // count: a stopped radio still has its stations.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_count(Some(23)));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(state_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn an_update_of_count_alone_leaves_track_and_identity_intact() {
        // Safety guarantee that the spontaneous `preset_count` announcement
        // depends on, from the radio after a successful admin-side save
        // (see `RadioSource::poll_notification`): a frame carrying only the
        // count must leave the current track and identity intact, and
        // still publish the state. Nothing checked this before this test.
        let (mut core, mut np_rx, state_rx, _d) = setup_metadata(vec![]);
        let id = serde_json::json!({"kind": "stream", "url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        // Baseline taken after identity is installed: only later changes
        // must be detected.
        np_rx.borrow_and_update();
        let track_before = state_rx.borrow().track.clone();

        core.handle_source_update("radio", update_with_count(Some(5)));

        assert_eq!(state_rx.borrow().preset_count, Some(5), "the count must be published");
        assert_eq!(state_rx.borrow().track, track_before, "the track must not move");
        assert!(!np_rx.has_changed().unwrap(), "the identity must not move");
        assert_eq!(np_rx.borrow().identity, Some(id));
    }

    #[tokio::test]
    async fn an_update_of_name_alone_leaves_track_and_identity_intact() {
        // Same guarantee as for `preset_count` above, this time for
        // `preset_name`: a frame carrying only the name must merge into the
        // published state without disturbing anything else.
        let (mut core, mut np_rx, state_rx, _d) = setup_metadata(vec![]);
        let id = serde_json::json!({"kind": "stream", "url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        np_rx.borrow_and_update();
        let track_before = state_rx.borrow().track.clone();

        core.handle_source_update("radio", update_with_name(Some("FIP")));

        assert_eq!(state_rx.borrow().preset_name.as_deref(), Some("FIP"), "the name must be published");
        assert_eq!(state_rx.borrow().track, track_before, "the track must not move");
        assert!(!np_rx.has_changed().unwrap(), "the identity must not move");
        assert_eq!(np_rx.borrow().identity, Some(id));
    }

    #[tokio::test]
    async fn count_is_forgotten_in_standby() {
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_count(Some(23)));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::Power).await.unwrap(); // enters standby
        assert_eq!(state_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn an_ephemeral_message_clears_and_lets_the_previous_state_reappear() {
        // Real case: selecting an empty preset. Nothing is launched, the
        // previous station is still playing — the message must therefore
        // show, then give way, without the permanent status or the
        // metadata moving.
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", plays(id.clone()));
        let mut permanent = bare_update();
        permanent.status = Some("FIP".into());
        core.handle_source_update("radio", permanent);
        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("FIP"));

        let mut ephemeral = bare_update();
        ephemeral.transient = true;
        // The displayed word comes from `status`, never from a composed
        // view (see Task 3): this is how the radio plugin actually
        // declares it on the "empty preset" branch.
        ephemeral.status = Some("empty preset".into());
        core.handle_source_update("radio", ephemeral);
        let during = state_rx.borrow_and_update().clone();
        assert!(matches!(during.overlay, Some(Overlay::Message { .. })), "the message must display");
        assert_eq!(during.status.as_deref(), Some("FIP"), "the permanent status has not moved");
        assert!(core.overlay_deadline().is_some(), "and carry a deadline");

        core.expire_overlay();
        let after = state_rx.borrow_and_update().clone();
        assert!(after.overlay.is_none());
        assert_eq!(after.status.as_deref(), Some("FIP"), "the station that is playing must reappear");
        assert_eq!(after.track.title.as_deref(), Some("So What"), "and so must the metadata");
    }

    #[tokio::test]
    async fn a_source_status_is_published_then_replaced() {
        // Convention **different** from `preset`'s: within a frame, an
        // absent `status` means "no status", not "keep the previous one".
        // This is what reproduces the current behavior — a source
        // recomposes its whole view on every frame — and the only
        // convention that allows clearing a status: otherwise "NO DISC"
        // would stay displayed after a disc is inserted, with no way to
        // cancel it.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut update = bare_update();
        update.status = Some("NO DISC".into());
        core.handle_source_update("radio", update);
        assert_eq!(core.player_state().status.as_deref(), Some("NO DISC"));

        core.handle_source_update("radio", bare_update());
        assert_eq!(core.player_state().status, None, "absent means cleared, not kept");
    }

    #[tokio::test]
    async fn an_ephemeral_status_does_not_touch_the_remembered_status() {
        // The "empty preset" case: a passing word, while the previous
        // station keeps playing. It feeds the overlay, and the permanent
        // status must reappear once the deadline passes.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("FIP".into());
        core.handle_source_update("radio", permanent);

        let mut ephemeral = bare_update();
        ephemeral.status = Some("EMPTY PRESET".into());
        ephemeral.transient = true;
        core.handle_source_update("radio", ephemeral);
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("FIP"),
            "the permanent status survives an ephemeral message"
        );
        assert!(matches!(core.player_state().overlay, Some(Overlay::Message { .. })));

        core.expire_overlay();
        assert_eq!(core.player_state().status.as_deref(), Some("FIP"));
        assert!(core.player_state().overlay.is_none());
    }

    #[tokio::test]
    async fn count_alone_does_not_clear_the_source_status() {
        // The defect was **in production**: `plugin-files` announces a
        // count with no status when its admin page saves a list, even
        // though it declares a permanent status everywhere else. The
        // status therefore disappeared from the console and the SPA until
        // the next command.
        //
        // `preset_count` has always been in the SDK's interesting-frame
        // predicate: this frame **reaches** the core, and status handling
        // would have cleared it for lack of one.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("6 FILES".into());
        core.handle_source_update("radio", permanent);
        assert_eq!(core.player_state().status.as_deref(), Some("6 FILES"));

        let mut count = bare_update();
        count.preset_count = Some(6);
        core.handle_source_update("radio", count);
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("6 FILES"),
            "a frame that declares neither identity nor status has nothing to say about the status"
        );
        assert_eq!(
            core.player_state().preset_count,
            Some(6),
            "and the count must still be taken: the early return is after it"
        );
    }

    #[tokio::test]
    async fn a_renumbering_notice_does_not_clear_the_status() {
        // The exact frame from `plugin-files` after a save from its admin
        // page: the count, **and** the number and name of the current
        // track, with neither identity (the track must not be redeclared)
        // nor status. Three merged fields, no view recomposition.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("6 FILES".into());
        core.handle_source_update("radio", permanent);

        let mut notice = bare_update();
        notice.preset_count = Some(9);
        notice.preset = Some(3);
        notice.preset_name = Some("Kind of Blue".into());
        core.handle_source_update("radio", notice);
        let state = core.player_state();
        assert_eq!(state.status.as_deref(), Some("6 FILES"), "the permanent status survives");
        assert_eq!(state.preset_count, Some(9));
        assert_eq!(state.preset, Some(3));
        assert_eq!(state.preset_name.as_deref(), Some("Kind of Blue"));
    }

    #[tokio::test]
    async fn presets_alone_do_not_clear_the_status() {
        // The second source of the same trap: the `ListPresets` response
        // carries neither identity nor status. Without the early return,
        // asking a source for its sources_catalog would blank its status.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("NO DISC".into());
        core.handle_source_update("radio", permanent);

        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("NO DISC"),
            "asking for the sources_catalog must not clear the screen"
        );
        assert_eq!(names(&core.sources_catalog()), vec!["cd".to_string(), "radio".into()]);
    }

    #[tokio::test]
    async fn presets_of_an_inactive_source_are_kept() {
        // The whole reason for the guard's bypass: `listplaylistinfo
        // "radio"` is polled while the cd is playing.
        let (mut core, _pc, _sc, _rx, _d) =
            setup_persisted(PersistedState { active_source: "cd".into(), ..Default::default() });
        assert_eq!(core.active_source(), "cd");
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]));
        let cat = core.sources_catalog();
        let radio = cat.sources.iter().find(|s| s.name == "radio").expect("radio is declared");
        assert_eq!(radio.presets, vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]);
        let cd = cat.sources.iter().find(|s| s.name == "cd").expect("cd is declared");
        assert!(cd.presets.is_empty(), "the cd lists nothing, it is still present");
    }

    #[tokio::test]
    async fn presets_arrive_even_in_standby() {
        // The guard stops identity and status, not a fact about a source:
        // what a source contains does not depend on the device being
        // powered on.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::Power).await.unwrap();
        assert!(core.player_state().standby, "the device is asleep");
        core.handle_source_update("radio", with_presets(vec![preset_of(3, "FIP")]));
        let cat = core.sources_catalog();
        let radio = cat.sources.iter().find(|s| s.name == "radio").unwrap();
        assert_eq!(radio.presets, vec![preset_of(3, "FIP")]);
    }

    // -- Partial state (`known`) and cover: task 5 -----------------------

    // -- Embedded CoverPayload, read by the core: task 6 ----------------------

    /// It is this function, and no longer a re-reading of `main`'s code,
    /// that proves the sharing required by task 5: `Core` and the HTTP
    /// `AppState` must receive **the same** `Arc<CoverCache>`. A second
    /// `Arc::new(CoverCache::new())` slipped in for either one would
    /// compile and let every other test through — including the HTTP route
    /// test above, which builds its own `AppState` by hand — but would
    /// break `Arc::ptr_eq` here.
    #[test]
    fn the_core_and_the_appstate_really_share_the_same_arc() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let wiring = Wiring {
            sources: HashMap::new(),
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
            metadata: silent_wiring(vec![]),
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
        };
        let (cover_tx, _cover_rx) = mpsc::channel::<(String, bool)>(4);
        let (app_state, core) = crate::assemble_covers_and_core(
            FakePlayer::default(),
            wiring,
            cover_tx,
            mpsc::channel(4).0,
            crate::status::tests_support::app_state(),
        );
        assert!(
            Arc::ptr_eq(core.app_covers(), &app_state.covers),
            "the core and the HTTP AppState must share the same Arc<CoverCache>"
        );
    }

}
