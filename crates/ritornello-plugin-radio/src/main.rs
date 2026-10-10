mod admin;
mod config;
mod directory;
// Only compiled under `cargo test`: `ui_placeholder_js` serves nowhere at
// run time in this crate (unlike the core's `placeholder_html`, used as a
// fallback by `web.rs`), only `build.rs` (separate compilation, via
// `include!`) and its own tests. Compiling it into the binary permanently
// would trigger a `dead_code` that `-D warnings` would refuse.
#[cfg(test)]
mod placeholder;
mod state;

use crate::admin::RadioAdmin;
use anyhow::Result;
use config::Stations;
use ritornello_plugin_sdk::{Notification, SourceOutcome, SourcePlugin};
use ritornello_proto::{Preset, SourceAction, Text};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

pub(crate) const RADIO_EN: &str = include_str!("locales/en.toml");

/// Where this plugin keeps its files, relative to its data directory.
fn paths_in(data: &std::path::Path) -> (PathBuf, PathBuf) {
    (data.join("stations.toml"), data.join("state.json"))
}

struct RadioSource {
    state_path: PathBuf,
    stations: Arc<AsyncRwLock<Stations>>,
    preset: u8,
    /// URL of the playing stream, when something is playing.
    ///
    /// The preset is a **position**: reshuffling the table from the page
    /// therefore made the memorized number point at another station, and the
    /// screen announced the wrong name for the stream that kept playing. The
    /// URL, on the other hand, durably identifies what is playing, and makes
    /// it possible to find the right number back in the reshuffled table.
    current_url: Option<String>,
    /// URL of the station kept armed while stopped, the stopped counterpart
    /// of `current_url`: a reshuffle of the table moves the armed station's
    /// number too, and Play starts the station at `preset`, so the number
    /// must follow the URL here as well or the screen and Play disagree.
    armed_url: Option<String>,
    /// Receives the new `Stations::preset_count()` announced by the Admin
    /// half after a successful save (see `RadioAdmin::set_data`). `main()`
    /// always builds this field as `Some`: the admin page is registered
    /// unconditionally with `Runtime`. `None` only appears in tests, which
    /// build `RadioSource` directly without going through `Runtime` and thus
    /// without an Admin half to emit on this channel; `poll_notification`
    /// then waits forever instead of returning `None`, which is **terminal**
    /// for the SDK.
    preset_count_rx: Option<tokio::sync::watch::Receiver<u8>>,
}

impl RadioSource {
    /// Identity of what the radio is playing: the stream, designated by its
    /// URL.
    ///
    /// Opaque to the core, which only compares and relays it. It is, on the
    /// other hand, what a `metadata` plugin reads to recognize a station: the
    /// URL is the only thing that durably distinguishes a stream (the preset
    /// name, for its part, depends on the device's configuration).
    fn stream_identity(url: &str) -> serde_json::Value {
        serde_json::json!({ "kind": "stream", "url": url })
    }

    /// What the source says while nothing plays: the station Play would
    /// start is kept **armed**, so its number and name stay on screen.
    ///
    /// The armed identity is exactly the one `play_preset` declares for the
    /// same station: the core keeps the slate of the stream that was playing
    /// when the two are equal. With no station at that number there is
    /// nothing to arm.
    async fn armed_outcome(&mut self) -> SourceOutcome {
        let stations = self.stations.read().await;
        let count = stations.preset_count();
        match stations.by_preset(self.preset) {
            Some(st) => {
                self.armed_url = Some(st.url.clone());
                SourceOutcome::new(SourceAction::Noop)
                    .armed(Self::stream_identity(&st.url))
                    .preset(self.preset)
                    .preset_name(st.name.clone())
                    .preset_count(count)
            }
            None => {
                self.armed_url = None;
                SourceOutcome::new(SourceAction::Noop).plays_nothing().preset_count(count)
            }
        }
    }

    /// Whether a stream is playing: `current_url` is set by `play_preset` and
    /// cleared by `stop` and `deactivate`, so it is exactly "playing".
    fn is_playing(&self) -> bool {
        self.current_url.is_some()
    }

    /// Moves the armed station to `n` without launching it, and persists it
    /// as `play_preset` does.
    async fn arm_preset(&mut self, n: u8) -> SourceOutcome {
        self.preset = n;
        if let Err(e) = state::update(&self.state_path, |s| s.preset = n) {
            tracing::warn!("failed to persist preset: {e}");
        }
        self.armed_outcome().await
    }

    async fn play_preset(&mut self, n: u8) -> SourceOutcome {
        let stations = self.stations.read().await;
        // How many numbered presets exist right now, for the web grid — see
        // `Stations::preset_count`. Declared on both branches below: a miss
        // (empty preset) still tells the truth about the table.
        let count = stations.preset_count();
        if let Some(st) = stations.by_preset(n) {
            self.preset = n;
            self.current_url = Some(st.url.clone());
            self.armed_url = None;
            // `update` and not `save`: the Admin half writes the chosen
            // country into this same file, and a `save` built here would
            // erase it. The failure is logged, as the Admin half already
            // does: a read-only /var/lib would lose the preset on every
            // reboot without anything saying so.
            if let Err(e) = state::update(&self.state_path, |s| s.preset = n) {
                tracing::warn!("failed to persist preset: {e}");
            }
            SourceOutcome::new(SourceAction::play(st.url.clone()))
                .plays_at(Self::stream_identity(&st.url), st.url.clone())
                // The key the UI must highlight: only the Source knows which
                // preset what is playing corresponds to.
                .preset(n)
                // The station's configured name: it is what the Player card
                // displays next to the preset number.
                .preset_name(st.name.clone())
                .preset_count(count)
        } else {
            // **Ephemeral** message: nothing was launched, so the previous
            // station is still playing and must reappear on screen. Leaving
            // it permanent durably described a state that did not exist.
            //
            // And above all, no identity declaration: `plays_nothing()` would
            // be false here, since the previous stream carries on — that
            // would have made the `metadata` plugins stop and blanked the
            // displayed title.
            SourceOutcome::new(SourceAction::Noop)
                .transient()
                .status_text(Text::Keyed { key: "empty_preset".into(), params: HashMap::new() })
                .preset_count(count)
        }
    }
}

#[async_trait::async_trait]
impl SourcePlugin for RadioSource {
    /// The icon the web UI shows this source under.
    fn icon(&self) -> Option<&'static str> {
        Some("radio")
    }

    async fn activate(&mut self) -> SourceOutcome {
        let preset = self.preset;
        self.play_preset(preset).await
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        // Nothing is playing anymore: forget the URL, otherwise a reshuffle
        // of the table would correct the preset of a stopped stream.
        self.current_url = None;
        self.armed_url = None;
        SourceOutcome::new(SourceAction::Stop).plays_nothing()
    }
    /// Stopped: the station stays on screen, armed. Play starts it again
    /// (`activate` plays `self.preset`).
    async fn stop(&mut self) -> SourceOutcome {
        // Same reason as `deactivate`: a reshuffle must not correct the
        // preset of a stream that is no longer playing.
        self.current_url = None;
        self.armed_outcome().await
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        self.play_preset(n).await
    }
    async fn next(&mut self) -> SourceOutcome {
        let next = self.stations.read().await.next_preset(self.preset);
        match next {
            // Only one station configured: next_preset loops back onto the
            // current preset. Replaying would cause a reconnection of the
            // live stream (mpv loadfile), audible as a station change while
            // the display does not move. Nothing to do in that case — and
            // above all say nothing about the identity, which has not
            // changed.
            Some(n) if n == self.preset => SourceOutcome::new(SourceAction::Noop),
            // Stopped: move the armed station, launch nothing.
            Some(n) if !self.is_playing() => self.arm_preset(n).await,
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome::new(SourceAction::Noop),
        }
    }
    async fn prev(&mut self) -> SourceOutcome {
        let prev = self.stations.read().await.prev_preset(self.preset);
        match prev {
            // See the comment in next(): same guard against the audible
            // reconnection when only one station is configured.
            Some(n) if n == self.preset => SourceOutcome::new(SourceAction::Noop),
            // Stopped: move the armed station, launch nothing.
            Some(n) if !self.is_playing() => self.arm_preset(n).await,
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome::new(SourceAction::Noop),
        }
    }
    async fn eject(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop)
    }
    /// The radio's named presets: its stations, under the `AsyncRwLock`
    /// shared with the Admin half. Only source overriding this method for
    /// now — the cd has no names by nature, and the file list is already the
    /// queue, not a set of presets.
    async fn list_presets(&mut self) -> Vec<Preset> {
        self.stations.read().await.presets()
    }

    /// Spontaneously announces the new `preset_count` when the Admin half has
    /// just saved a station table — this is what updates the web remote's
    /// grid without waiting for a preset to be played (defect observed in
    /// use: the grid stayed on the old set of numbers until then).
    ///
    /// It also corrects the **number and name** of what is playing when the
    /// reshuffle moved them: the preset is a position, so reordering the
    /// stations made the name of another station be announced for the stream
    /// that kept playing, and a reboot resumed the wrong one. The stream is
    /// found back by its URL, the only thing that durably identifies it.
    ///
    /// It also republishes the named presets (`presets`): the table having
    /// just been re-saved, this is what propagates a station's renaming
    /// without an MPD client having to ask for it again.
    ///
    /// Carries **no status, no identity, and no play request**: the radio
    /// plays a single stream, there is nothing to reload, only the record to
    /// set straight — and the sound is not interrupted. `presets`, `preset`,
    /// `preset_name` and `preset_count` are facts about the source, not a
    /// status or an identity: that is precisely what keeps this frame out of
    /// the erasure guard described below.
    ///
    /// Beware: `Core::handle_source_update` does **not** merge everything,
    /// contrary to what this spot used to claim. `preset`, `preset_name` and
    /// `preset_count` are indeed kept when absent, but `status` is *replaced*
    /// by what the frame carries, absence included (`if !update.transient {
    /// self.source_status = update.status.clone(); }`): it is the only
    /// convention that makes erasing a status possible. This notification
    /// erases none, however, because the core returns **before** that
    /// handling for a frame that declares neither identity nor status. The
    /// radio never declaring a permanent status, the defect was invisible
    /// here; it was quite real in `plugin-files`, which declares one.
    async fn poll_notification(&mut self) -> Option<Notification> {
        let Some(rx) = &mut self.preset_count_rx else {
            // Only happens in tests (see the comment on the field): `main()`
            // always builds this receiver. Never `None` here, which would be
            // terminal for the SDK.
            return std::future::pending().await;
        };
        match rx.changed().await {
            Ok(()) => {
                let n = *rx.borrow_and_update();
                let mut notice = Notification::new().preset_count(n);
                // Same path as `preset_count`: the table has just been
                // reshuffled (admin page), so republish the named presets
                // alongside, so that a renamed station propagates without
                // being asked for again. An empty list is not declared: it is
                // the same statement as absence (see `SourceOutcome::presets`),
                // and a frame that carries only that must not claim a fact it
                // does not have.
                let presets = self.stations.read().await.presets();
                if !presets.is_empty() {
                    notice = notice.presets(presets);
                }
                // The table has just been reshuffled: find out **where the
                // playing stream went**, and correct the displayed number and
                // name.
                //
                // Without this, the preset being a position, reordering the
                // stations made the name of another station be announced for
                // the stream that kept playing — and a reboot resumed the
                // wrong one.
                //
                // No action here, and rightly so: the radio plays a single
                // stream, there is nothing to reload, only the record to set
                // straight.
                // Playing: the stream; stopped: the armed station.
                if let Some(url) = self.current_url.clone().or_else(|| self.armed_url.clone()) {
                    let stations = self.stations.read().await;
                    // Station removed from the table: its number no longer
                    // designates anything reliable, and the protocol has no
                    // "no presets left". Playing, we then refrain from lying
                    // further by touching nothing: the stream goes on.
                    if let Some(st) = stations.by_url(&url) {
                        let (p, name) = (st.preset, st.name.clone());
                        drop(stations);
                        if p != self.preset {
                            self.preset = p;
                            // Persist too: otherwise a reboot would resume
                            // the pre-reshuffle number, hence another
                            // station.
                            if let Err(e) = state::update(&self.state_path, |s| s.preset = p) {
                                tracing::warn!("failed to persist preset: {e}");
                            }
                        }
                        notice = notice.preset(p).preset_name(name);
                    } else if !self.is_playing() {
                        // Stopped, and the armed station is gone: the screen
                        // would keep naming a station Play can no longer
                        // start, while Play starts whatever now sits at
                        // `self.preset`. Re-arm from that number — the
                        // station there, or nothing if the number is empty.
                        // Nothing plays, so declaring an identity interrupts
                        // nothing.
                        match stations.by_preset(self.preset) {
                            Some(st) => {
                                self.armed_url = Some(st.url.clone());
                                notice = notice
                                    .armed(Self::stream_identity(&st.url))
                                    .preset(self.preset)
                                    .preset_name(st.name.clone());
                            }
                            None => {
                                self.armed_url = None;
                                notice = notice.plays_nothing();
                            }
                        }
                    }
                }
                Some(notice)
            }
            // The sender (Admin half) vanished — should not happen in
            // practice as long as both halves share the same process, but
            // nothing justifies treating that as a definitive end of
            // notifications: we fall back onto the same indefinite wait as
            // the field's `None` branch rather than returning `None`
            // ourselves.
            Err(_) => std::future::pending().await,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    // Everything this plugin writes lives in its own data directory — see
    // `ritornello_plugin_sdk::DATA_DIR_ENV` for why there is exactly one.
    let (stations_path, state_path) = paths_in(&ritornello_plugin_sdk::data_dir());

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalid or missing ({e}): starting without stations");
        Stations::default()
    });
    let preset = state::load(&state_path).preset;
    let stations_shared = Arc::new(AsyncRwLock::new(stations));

    // Admin -> Source channel for the spontaneous `preset_count` announcement
    // (see `RadioAdmin::set_data` and `RadioSource::poll_notification`). The
    // initial value is never used: only later changes count, the startup
    // count is already carried by `activate`/`select`.
    let (preset_count_tx, preset_count_rx) = tokio::sync::watch::channel(0u8);

    let source = RadioSource {
        state_path: state_path.clone(),
        stations: stations_shared.clone(),
        preset,
        // Nothing is playing yet: filled in at the first `Play`.
        current_url: None,
        armed_url: None,
        // The receiver only makes sense if an Admin half exists to emit on
        // it (see below): otherwise `poll_notification` must wait forever,
        // not fall back onto a dead channel.
        preset_count_rx: Some(preset_count_rx),
    };
    // Online directory: the built-in server list, tried in order until the
    // first one that answers, or the single server pinned by
    // `RITORNELLO_RADIO_DIRECTORY`. Logged at startup: on a headless Pi,
    // knowing which servers will be queried saves the guessing.
    let directory = directory::HttpDirectory::from_env();
    tracing::info!("radio directory, candidate servers: {}", directory.bases.join(", "));
    let admin = RadioAdmin {
        stations_path,
        state_path,
        stations: stations_shared,
        directory: Arc::new(directory),
        search: RwLock::new(Vec::new()),
        countries: RwLock::new(Vec::new()),
        preset_count_tx,
    };
    ritornello_plugin_sdk::declare_runtime!()?
        .texts([("en", RADIO_EN)])?
        .source(source)?
        .admin(admin)?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_plugin_sdk::AdminPlugin;

    #[test]
    fn every_file_lives_in_the_plugin_s_own_directory() {
        let d = std::path::Path::new("/x/plugins/radio");
        let (stations, state) = paths_in(d);
        assert!(stations.starts_with(d) && state.starts_with(d));
        assert_eq!(stations.file_name().unwrap(), "stations.toml");
        assert_eq!(state.file_name().unwrap(), "state.json");
    }

    #[tokio::test]
    async fn selecting_an_empty_preset_carries_a_key_not_a_resolved_text() {
        // The present defect this task removes (language-packs chantier,
        // task 10): the plugin used to resolve its own status into a
        // finished string through a `Catalog` it kept in step with
        // `set_locale` — a method that never re-emitted anything on its
        // own. Fed from the event (selecting a preset that does not exist),
        // not through a direct call to a private method.
        let mut source = make_source(Stations::default(), 1);
        // no preset loaded → "empty_preset" branch
        let outcome = source.select(1).await;
        assert_eq!(
            outcome.status_text,
            Some(Text::Keyed { key: "empty_preset".into(), params: HashMap::new() }),
            "the frame must carry a key for the core to resolve, not a finished string"
        );
    }

    /// The icon the web UI shows this source under.
    #[test]
    fn it_announces_its_icon() {
        assert_eq!(make_source(Stations::default(), 1).icon(), Some("radio"));
    }

    #[test]
    fn embedded_en_for_radio_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(RADIO_EN).unwrap().is_empty());
    }

    fn make_source(stations: Stations, preset: u8) -> RadioSource {
        let dir = tempfile::tempdir().unwrap();
        RadioSource {
            state_path: dir.path().join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(stations)),
            preset,
            current_url: None,
            armed_url: None,
            preset_count_rx: None,
        }
    }

    fn one_station() -> Stations {
        config::Stations {
            stations: vec![config::Station {
                name: "FIP".into(),
                url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(),
                preset: 1,
            }],
        }
    }

    fn two_stations() -> Stations {
        config::Stations {
            stations: vec![
                config::Station {
                    name: "FIP".into(),
                    url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(),
                    preset: 1,
                },
                config::Station {
                    name: "France Inter".into(),
                    url: "http://icecast.radiofrance.fr/franceinter-midfi.mp3".into(),
                    preset: 2,
                },
            ],
        }
    }

    #[tokio::test]
    async fn with_a_single_station_next_and_prev_have_no_effect() {
        let mut source = make_source(one_station(), 1);
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        // Say nothing about the identity: the station has not changed, and
        // announcing a change would reset the current track's metadata.
        assert!(outcome.identity.is_none());

        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.identity.is_none());
    }

    #[tokio::test]
    async fn playing_a_station_declares_its_stream_as_identity() {
        let mut source = make_source(two_stations(), 1);
        let outcome = source.select(2).await;
        assert_eq!(
            outcome.identity,
            Some(ritornello_proto::IdentityUpdate::Playing(serde_json::json!({
                "kind": "stream",
                "url": "http://icecast.radiofrance.fr/franceinter-midfi.mp3"
            })))
        );
        assert_eq!(
            outcome.location.as_deref(),
            Some("http://icecast.radiofrance.fr/franceinter-midfi.mp3"),
            "the location is the station's configured stream URL"
        );
    }

    #[tokio::test]
    async fn playing_a_preset_declares_it_for_the_ui() {
        // This is what lets the web remote highlight the active key: only
        // the Source knows which preset what is playing corresponds to.
        let mut source = make_source(two_stations(), 1);
        let outcome = source.select(2).await;
        assert_eq!(outcome.preset, Some(2));
        // The station's configured name always accompanies the number.
        assert_eq!(outcome.preset_name.as_deref(), Some("France Inter"));
        // The preset count (here 2, the highest of two_stations) is declared
        // on the "found" branch.
        assert_eq!(outcome.preset_count, Some(2));
        // And an empty preset declares neither preset nor name: what is
        // playing has not changed, the previous station carries on.
        let outcome = source.select(7).await;
        assert_eq!(outcome.preset, None);
        assert_eq!(outcome.preset_name, None, "no name on the empty branch: nothing changed");
        // ... but the count is still declared on the "empty" branch too: the
        // table has not changed, only the selection failed.
        assert_eq!(outcome.preset_count, Some(2));
    }

    #[tokio::test]
    async fn an_empty_preset_shows_an_ephemeral_message_without_cutting_playback() {
        // Defect observed in use: the message stayed on screen indefinitely.
        // Yet nothing was launched — the previous station is still playing —
        // so the display must come back to it, and above all the metadata
        // must not be erased.
        let mut source = make_source(Stations::default(), 1);
        let outcome = source.select(4).await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.transient, "the message must clear by itself");
        assert!(
            outcome.identity.is_none(),
            "declaring a stop would be false: the previous stream carries on"
        );
        // The ephemeral word is declared via `status_text`: it is what feeds
        // the overlay on the core side, once resolved.
        assert_eq!(
            outcome.status_text,
            Some(Text::Keyed { key: "empty_preset".into(), params: HashMap::new() })
        );
        // Empty table: the declared count is 0, not absent.
        assert_eq!(outcome.preset_count, Some(0));
    }

    #[tokio::test]
    async fn deactivating_declares_that_nothing_plays_anymore() {
        let mut source = make_source(two_stations(), 1);
        let outcome = source.deactivate().await;
        assert!(matches!(outcome.action, SourceAction::Stop));
        assert_eq!(outcome.identity, Some(ritornello_proto::IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn with_two_stations_next_and_prev_always_wrap_to_the_other() {
        // Playing, as in use: next and previous only launch while something
        // plays (stopped, they arm: see the `*_while_stopped_*` tests).
        let mut source = make_source(two_stations(), 1);
        source.activate().await;
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));

        let mut source = make_source(two_stations(), 1);
        source.activate().await;
        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));
    }

    #[tokio::test]
    async fn activate_on_the_current_preset_still_replays_the_stream() {
        // Recovery path after an outage (retry_stream on the core side):
        // activate() must keep replaying the same preset, without the guard
        // added to next()/prev().
        let mut source = make_source(one_station(), 1);
        let outcome = source.activate().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));
    }

    #[tokio::test]
    async fn poll_notification_touches_neither_identity_nor_sound() {
        // Safety property: the spontaneous announcement (save from the admin
        // page) must neither cut the stream nor change what the `metadata`
        // plugins believe they hear. Nothing is playing here, so no preset to
        // correct either.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        tx.send(5).unwrap();

        let n = source.poll_notification().await.expect("notification expected");
        assert_eq!(n.preset_count, Some(5));
        assert!(n.identity.is_none(), "the current track must not move");
        assert!(n.location.is_none(), "the current location must not move");
        assert!(n.preset.is_none(), "nothing is playing: no number to correct");
    }

    #[tokio::test]
    async fn a_table_reshuffle_corrects_the_number_of_what_is_playing() {
        // Reported design defect: the preset is a **position**. Reordering
        // the stations from the page made the memorized number point at
        // another station — the screen announced the wrong name for the
        // stream that kept playing, and a reboot resumed the wrong one. The
        // stream is found back by its URL, the only thing that durably
        // identifies it.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        // Station 1 is playing.
        let url = match source.activate().await.action {
            SourceAction::Play { uri, .. } => uri,
            other => panic!("expected a Play, got {other:?}"),
        };

        // The page reshuffles the table: this same stream moves to preset 2.
        {
            let mut st = source.stations.write().await;
            for s in st.stations.iter_mut() {
                s.preset = if s.url == url { 2 } else { 1 };
            }
        }
        tx.send(2).unwrap();

        let n = source.poll_notification().await.expect("notification expected");
        assert_eq!(n.preset, Some(2), "the number must follow the station");
        assert!(n.preset_name.is_some(), "and the name with it");
        assert_eq!(source.preset, 2, "memorized, so that next/prev start from there");
        // And above all: no action, so the stream is not cut because of it.
        assert!(n.identity.is_none(), "the stream's identity has not changed");
    }

    #[tokio::test]
    async fn a_removed_station_does_not_make_up_a_number() {
        // Its number no longer designates anything reliable, and the protocol
        // has no "no presets left": better to say nothing than to designate a
        // station at random.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        source.activate().await;
        source.stations.write().await.stations.clear();
        tx.send(0).unwrap();

        let n = source.poll_notification().await.expect("notification expected");
        assert_eq!(n.preset_count, Some(0));
        assert!(n.preset.is_none(), "no number made up");
    }

    #[tokio::test]
    async fn the_initial_value_of_a_fresh_watch_is_not_seen_as_a_change() {
        // Pillar `poll_notification` depends on: a freshly created
        // `watch::channel(v).1` never signals its starting value as a change
        // for `changed()`. If this property stopped being true — or if the
        // wiring went through `subscribe()` then `mark_changed()`, or moved
        // the channel creation elsewhere — every radio startup would announce
        // `preset_count(0)` before playback even starts: empty grid and
        // "Presets: 0" until something plays.
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(tokio::sync::watch::channel(0u8).1);
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            source.poll_notification(),
        )
        .await;
        assert!(
            result.is_err(),
            "the watch's initial value must produce no notification"
        );
    }

    #[tokio::test]
    async fn without_an_admin_half_poll_notification_keeps_waiting() {
        // Source built directly, without going through `Runtime` (as this
        // test does), hence without a `preset_count` announcement channel: no
        // sender exists, so nothing must ever come out of it — least of all a
        // `None`, terminal for the SDK (see the comment on the
        // `preset_count_rx` field). `main()`, for its part, always registers
        // the admin page and therefore always provides this channel.
        let mut source = make_source(two_stations(), 1);
        assert!(source.preset_count_rx.is_none());
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            source.poll_notification(),
        )
        .await;
        assert!(result.is_err(), "poll_notification should never have finished");
    }

    #[tokio::test]
    async fn list_presets_reads_the_table_under_the_shared_lock() {
        // Checks the wiring of `SourcePlugin::list_presets` onto
        // `Stations::presets` (already covered by its own tests in
        // `config.rs`): here, it is the trip through the shared lock that is
        // under test, not the sorting.
        let mut source = make_source(two_stations(), 1);
        assert_eq!(
            source.list_presets().await,
            vec![
                Preset { index: 1, name: "FIP".into() },
                Preset { index: 2, name: "France Inter".into() },
            ]
        );
    }

    #[tokio::test]
    async fn saving_the_stations_propagates_the_new_list() {
        // Same channel as `preset_count` (see the doc of `poll_notification`):
        // the admin and the source share the same table and the same channel,
        // so a successful save must make the new named list appear without a
        // client asking for it again.
        let dir = tempfile::tempdir().unwrap();
        let stations_shared = Arc::new(AsyncRwLock::new(one_station()));
        let (tx, rx) = tokio::sync::watch::channel(0u8);

        let mut admin = RadioAdmin {
            stations_path: dir.path().join("stations.toml"),
            state_path: dir.path().join("plugin-radio.json"),
            stations: stations_shared.clone(),
            directory: Arc::new(crate::directory::HttpDirectory::from_env()),
            search: RwLock::new(Vec::new()),
            countries: RwLock::new(Vec::new()),
            preset_count_tx: tx,
        };
        let mut source = RadioSource {
            state_path: dir.path().join("plugin-radio.json"),
            stations: stations_shared,
            preset: 1,
            current_url: None,
            armed_url: None,
            preset_count_rx: Some(rx),
        };

        let new_table = serde_json::json!({
            "op": "save",
            "stations": [{
                "name": "FIP renommée",
                "url": "http://icecast.radiofrance.fr/fip-midfi.mp3",
                "preset": 1,
            }]
        });
        admin.set_data(new_table).await.expect("valid save");

        let n = source.poll_notification().await.expect("notification expected");
        assert_eq!(n.presets, Some(vec![Preset { index: 1, name: "FIP renommée".into() }]));
    }

    const FIP: &str = "http://icecast.radiofrance.fr/fip-midfi.mp3";
    const INTER: &str = "http://icecast.radiofrance.fr/franceinter-midfi.mp3";

    /// A source on station 1 of two, with a state file the test can read
    /// back, that has been playing since `activate`.
    async fn playing_source() -> (RadioSource, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut source = make_source(two_stations(), 1);
        source.state_path = dir.path().join("plugin-radio.json");
        assert!(matches!(source.activate().await.action, SourceAction::Play { .. }));
        (source, dir)
    }

    fn armed_identity(outcome: &SourceOutcome) -> Option<serde_json::Value> {
        outcome.armed.as_ref().map(|a| a.identity.clone())
    }

    #[tokio::test]
    async fn stop_arms_the_current_station() {
        let (mut source, _dir) = playing_source().await;
        let outcome = source.stop().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        // The identity is exactly the one `play_preset` declared, so the core
        // keeps the slate (title, cover) of the stream that was playing.
        assert_eq!(armed_identity(&outcome), Some(RadioSource::stream_identity(FIP)));
        assert_eq!(outcome.preset, Some(1));
        assert_eq!(outcome.preset_name.as_deref(), Some("FIP"));
        assert_eq!(outcome.preset_count, Some(2));
        assert!(source.current_url.is_none(), "stopped: a reshuffle must not correct a stream");
    }

    #[tokio::test]
    async fn stop_without_a_station_plays_nothing_and_arms_nothing() {
        let mut source = make_source(Stations::default(), 1);
        let outcome = source.stop().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.armed.is_none());
        assert_eq!(outcome.identity, Some(ritornello_proto::IdentityUpdate::Nothing));
        assert_eq!(outcome.preset_count, Some(0));
    }

    /// While stopped, next moves the armed station and launches nothing.
    ///
    /// **[MUTATION]** dropping the `playing` test in `next()` turns this
    /// into a `Play` and fails the `Noop` assertion; the same for `prev()`
    /// in `prev_while_stopped_arms_the_previous_station_without_playing`.
    #[tokio::test]
    async fn next_while_stopped_arms_the_next_station_without_playing() {
        let (mut source, dir) = playing_source().await;
        source.stop().await;
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Noop), "nothing must start");
        assert_eq!(armed_identity(&outcome), Some(RadioSource::stream_identity(INTER)));
        assert_eq!(outcome.preset, Some(2));
        assert_eq!(outcome.preset_name.as_deref(), Some("France Inter"));
        assert_eq!(source.preset, 2);
        assert_eq!(state::load(&dir.path().join("plugin-radio.json")).preset, 2);
        assert!(source.current_url.is_none(), "still nothing playing");
    }

    #[tokio::test]
    async fn prev_while_stopped_arms_the_previous_station_without_playing() {
        let (mut source, dir) = playing_source().await;
        source.stop().await;
        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Noop), "nothing must start");
        assert_eq!(armed_identity(&outcome), Some(RadioSource::stream_identity(INTER)));
        assert_eq!(source.preset, 2);
        assert_eq!(state::load(&dir.path().join("plugin-radio.json")).preset, 2);
    }

    #[tokio::test]
    async fn next_while_stopped_with_a_single_station_changes_nothing() {
        let mut source = make_source(one_station(), 1);
        source.activate().await;
        source.stop().await;
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.armed.is_none() && outcome.identity.is_none());
    }

    #[tokio::test]
    async fn play_after_next_while_stopped_starts_the_armed_station() {
        let (mut source, _dir) = playing_source().await;
        source.stop().await;
        source.next().await;
        match source.activate().await.action {
            SourceAction::Play { uri, .. } => assert_eq!(uri, INTER),
            other => panic!("expected a Play, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn next_while_playing_still_plays() {
        let (mut source, _dir) = playing_source().await;
        match source.next().await.action {
            SourceAction::Play { uri, .. } => assert_eq!(uri, INTER),
            other => panic!("expected a Play, got {other:?}"),
        }
    }

    /// A reshuffle while stopped moves the armed station's number: the
    /// notification must follow it by URL, or the screen says "2 - FIP"
    /// while Play starts whatever now sits at 2.
    ///
    /// **[MUTATION]** dropping `.or_else(|| self.armed_url.clone())` in
    /// `poll_notification` fails the `Some(1)` assertion.
    #[tokio::test]
    async fn a_reshuffle_while_stopped_moves_the_armed_station_s_number() {
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let (mut source, _dir) = playing_source().await;
        source.preset_count_rx = Some(rx);
        source.stop().await;
        source.next().await; // armed: France Inter, preset 2
        {
            let mut st = source.stations.write().await;
            for s in st.stations.iter_mut() {
                s.preset = if s.url == INTER { 1 } else { 2 };
            }
        }
        tx.send(2).unwrap();
        let n = source.poll_notification().await.expect("notification expected");
        assert_eq!(n.preset, Some(1), "the number follows the armed station");
        assert_eq!(n.preset_name.as_deref(), Some("France Inter"));
        assert!(n.identity.is_none(), "the armed identity (the URL) has not changed");
        match source.activate().await.action {
            SourceAction::Play { uri, .. } => assert_eq!(uri, INTER),
            other => panic!("expected a Play, got {other:?}"),
        }
    }

    /// The armed station deleted from the page while stopped: the screen
    /// must re-arm what Play now starts — the station at the same number —
    /// and arm nothing once that number is empty too.
    ///
    /// **[MUTATION]** the `else if !self.is_playing()` branch removed from
    /// `poll_notification` → the `armed` assertion fires. Its
    /// `!self.is_playing()` replaced by `true` →
    /// `a_deleted_station_while_playing_redeclares_nothing` fires.
    #[tokio::test]
    async fn the_armed_station_deleted_while_stopped_re_arms_the_number() {
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let (mut source, _dir) = playing_source().await;
        source.preset_count_rx = Some(rx);
        source.stop().await;
        source.next().await; // armed: France Inter, preset 2
        let removed = {
            let mut st = source.stations.write().await;
            let fip = st.stations.iter().find(|s| s.url == FIP).cloned().expect("FIP is in the table");
            // France Inter goes; FIP takes its number.
            st.stations.retain(|s| s.url != INTER);
            st.stations.iter_mut().for_each(|s| s.preset = 2);
            fip
        };
        tx.send(1).unwrap();
        let n = source.poll_notification().await.expect("notification expected");
        let armed = n.armed.expect("the armed station is gone: what Play starts must be armed");
        assert_eq!(armed.identity, RadioSource::stream_identity(&removed.url));
        assert_eq!(n.preset, Some(2));
        assert_eq!(n.preset_name.as_deref(), Some(removed.name.as_str()));
        match source.activate().await.action {
            SourceAction::Play { uri, .. } => assert_eq!(uri, FIP, "Play starts what the screen shows"),
            other => panic!("expected a Play, got {other:?}"),
        }

        // Stopped again, armed on FIP at 2; then the table empties.
        source.stop().await;
        source.stations.write().await.stations.clear();
        tx.send(0).unwrap();
        let n = source.poll_notification().await.expect("notification expected");
        assert!(n.armed.is_none(), "nothing left to arm");
        assert_eq!(n.identity, Some(ritornello_proto::IdentityUpdate::Nothing));
    }

    /// Playing, a deleted station changes nothing: the stream goes on, and
    /// redeclaring anything would interrupt or blank it.
    #[tokio::test]
    async fn a_deleted_station_while_playing_redeclares_nothing() {
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let (mut source, _dir) = playing_source().await;
        source.preset_count_rx = Some(rx);
        let playing = source.current_url.clone().expect("something plays");
        source.stations.write().await.stations.retain(|s| s.url != playing);
        tx.send(1).unwrap();
        let n = source.poll_notification().await.expect("notification expected");
        assert!(n.identity.is_none() && n.armed.is_none(), "the stream goes on");
    }
}
