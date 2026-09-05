//! Events of the mpv player: end of playback, restart with growing backoff, resume on wake, and the request relayed to the active source.

use super::*;

impl<P: Player> Core<P> {
    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        if let Some(locale) = self.locale.clone() {
            for name in self.source_order.clone() {
                if let Some(src) = self.sources.get(&name)
                    && let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await
                {
                    tracing::warn!("SetLocale to {name}: {e}");
                }
            }
        }
        if let Some(action) = self.active_request(SourceReq::Wake).await? {
            self.apply(action).await?;
        }
        // The UI must know the volume and the source from the first display,
        // without waiting for something to be touched.
        self.publish_state();
        Ok(())
    }

    /// Replays the current content of the active source (`Activate` asks the
    /// source to give the current URI again, not to move to the next content).
    pub async fn retry_stream(&mut self) -> Result<()> {
        if !self.standby && self.expecting_stream
            && let Some(action) = self.active_request(SourceReq::Activate).await?
        {
            self.apply(action).await?;
        }
        Ok(())
    }

    pub async fn handle_event(&mut self, ev: Event) -> EventOutcome {
        match ev {
            // A single place decides which variants attest the liveness of
            // the stream: the `main` loop (which holds the `retry_at`
            // deadline) and this counter follow the same verdict via
            // `StreamAlive`, instead of duplicating the list of variants on
            // both sides.
            Event::Title(_) | Event::PlaybackActive => {
                self.retry_count = 0;
                return EventOutcome::StreamAlive;
            }
            // Deliberately without effect on `retry_count`: the liveness of
            // the stream is already attested by `PlaybackActive`, and an ICY
            // title is not a proof of playback (a station can send one then
            // go silent). Here, metadata only.
            Event::IcyTitle(title) => self.handle_icy_title(title),
            // Same status as ICY with respect to `retry_count`: metadata does
            // not prove that playback is alive.
            Event::FileTags(track) => self.handle_file_tags(*track),
            // Same status as the tags with respect to `retry_count`: the path
            // attests nothing about the liveness of the stream, it only
            // serves the embedded cover.
            Event::Path(path) => self.handle_path(path),
            // The player changed track on its own: end of a disc track, no
            // key pressed. The core knows it (mpv tells it) but cannot fix
            // the identity — it is opaque to it. So it tells the Source,
            // which will send view and identity back through the usual
            // channel. Without this, the display and the metadata stayed on
            // the previous track until the next command.
            //
            // The event also arrives for **requested** changes (the Source
            // has just realigned itself): it then sends back the same
            // identity, which the core recognizes as unchanged, and the
            // identical view is not pushed again.
            Event::TrackChanged(n) => {
                if !self.standby
                    && let Err(e) = self.active_request(SourceReq::PlayerTrack(n)).await
                {
                    tracing::debug!("track notification to source: {e}");
                }
            }
            Event::PlaybackIdle => {
                if !self.standby && self.expecting_stream {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    return EventOutcome::RetryIn(delay);
                }
                // Eof of **normal** playback (end of disc, notably): tell the
                // Source, the only one able to realign its playback state,
                // its view and its identity — the core cannot invent
                // "nothing plays anymore" in its place, the identity is
                // opaque. Without this, the end of a disc left the last
                // track and its metadata displayed indefinitely.
                // Idempotent when the stop comes from a command (the Source
                // has already been told by `Command::Stop`).
                //
                // Nothing plays anymore: without this, the tags of the last
                // file would remain admissible and a final refresh from mpv
                // would put them back on screen after the end of the list.
                self.playback = false;
                if !self.standby
                    && let Err(e) = self.active_request(SourceReq::Stop).await
                {
                    tracing::debug!("stop notification to source: {e}");
                }
            }
        }
        EventOutcome::Nothing
    }

    /// Request to the active source, **if there is one**.
    ///
    /// `Ok(None)` is not an error: since hotplug registration, the core can
    /// run without any source. A `source` plugin that misses the rendezvous
    /// window announces itself at t+30 s and is wired without a restart,
    /// and refusing to start at t+10 s to wait for it removed the status
    /// page precisely when one wanted to see it frozen there.
    ///
    /// This is what the former `panic!("unknown active source")` forbade:
    /// it protected no invariant — `Core::new` already falls back on the
    /// first sorted source, so the name is only unfindable if the table is
    /// **empty** — and it would have traded a readable refusal to start for
    /// a brutal crash at startup, with no page to tell the story.
    ///
    /// Without a source, a command **does nothing** and says so at `debug`:
    /// this is not an anomaly, only a device that has nothing to read.
    /// A `warn` would fill the UI's error buffer at every keypress.
    pub(super) async fn active_request(&self, req: SourceReq) -> Result<Option<SourceAction>> {
        let Some(source) = self.sources.get(&self.active_source) else {
            tracing::debug!("no active source, dropping {req:?}");
            return Ok(None);
        };
        source.request(req).await.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn resume_activates_the_persisted_source() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Wake"));
    }

    #[tokio::test]
    async fn resume_sends_wake_not_activate() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:Wake"));
        assert!(!calls.iter().any(|c| c == "radio:Activate"));
    }

    #[tokio::test]
    async fn resume_without_any_source_publishes_the_state_instead_of_panicking() {
        // The first caller of the active source at startup, and therefore
        // the first to die: `active()` panicked on an empty table, and
        // `resume` runs before the web server has served a single page. A
        // `panic!` there would have removed the status page precisely when
        // one wanted to see the frozen plugins on it.
        let (mut core, mut state_rx, dir) = setup_without_source();
        core.resume().await.unwrap();
        let state = state_rx.borrow_and_update().clone();
        assert_eq!(state.source, "", "the empty string IS the absence, naming it is up to the rendering");
        assert!(!state.standby, "the core starts, it does not enter standby for all that");
        drop(dir);
    }

    #[tokio::test]
    async fn intentional_stop_does_not_trigger_a_retry() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
    }

    #[tokio::test]
    async fn growing_backoff_then_reset_by_a_title() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        let d1 = restart(core.handle_event(Event::PlaybackIdle).await);
        let d2 = restart(core.handle_event(Event::PlaybackIdle).await);
        assert!(d2 > d1);
        // A title attests the liveness of the stream: it is also the verdict
        // the `main` loop follows to cancel the restart deadline.
        assert_eq!(core.handle_event(Event::Title("ok".into())).await, EventOutcome::StreamAlive);
        let d3 = restart(core.handle_event(Event::PlaybackIdle).await);
        assert_eq!(d3, d1);
    }

    #[tokio::test]
    async fn wake_noop_does_not_trigger_a_retry_cd_stays_silent() {
        // Regression (final review 2.2): the cd answers Noop to Wake (no
        // playback at boot/wake). The old retry gate (!stopped) still let a
        // restart be scheduled on the next PlaybackIdle, which made the cd
        // start on its own ~2s later. With expecting_stream, no Play was
        // emitted => no retry.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: Arc::new(Mutex::new(Vec::new())) }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
    }

    #[tokio::test]
    async fn finite_content_does_not_arm_the_restart_a_live_stream_does() {
        // Measured on the mpv 0.37 bench: at the end of a file list, mpv
        // goes `idle` exactly as during a stream cut. As long as the core
        // sniffed the URI (`cdda://`), a file path fell on the wrong side —
        // exponential restart instead of a clean stop, and the list started
        // over in a loop from the first track.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("/var/lib/ritornello/plugin-files.m3u").finite())
            .await
            .unwrap();
        assert!(!core.expecting_stream, "finite content must not arm the restart");

        core.apply(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
        assert!(core.expecting_stream, "a live stream must stay restartable");
    }

    #[tokio::test]
    async fn a_list_is_loaded_by_load_list_then_positioned() {
        // The defect this test should have caught, and now catches.
        //
        // With `loadfile`, mpv only unfolds an `.m3u` **afterwards**: measured
        // on mpv 0.37, `playlist-count` is 1, then 3 only after an
        // `end-file`/`start-file`. The chained `playlist-pos` therefore
        // arrived out of bounds, playback started over from the first track,
        // and the display lost preset and title. `loadlist` unfolds on the
        // spot — its answer even carries `num_entries` — which makes this
        // chaining safe.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(
            SourceAction::play("/var/lib/ritornello/plugin-files.m3u")
                .playlist()
                .starting_at(4)
                .finite(),
        )
        .await
        .unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec![
                "load_list /var/lib/ritornello/plugin-files.m3u".to_string(),
                "playlist-pos 4".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn a_media_is_still_loaded_by_loadfile() {
        // The distinction is declared by the Source, never guessed from the
        // URI: an `.m3u8` is a list for a file player and an HLS stream for
        // a radio. Sniffing the extension would break one of the two.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://icecast/fip.m3u8")).await.unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["play http://icecast/fip.m3u8".to_string()]
        );
    }

    #[tokio::test]
    async fn a_play_without_index_positions_nothing() {
        // The radio path: no superfluous command on the mpv socket.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["play http://icecast/fip.mp3".to_string()]);
    }

    #[tokio::test]
    async fn wake_play_does_trigger_a_retry_after_idle() {
        // Contrast with the previous test: when Wake results in Play (radio),
        // a stream is indeed expected, so a PlaybackIdle must schedule a retry.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(matches!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::RetryIn(_)));
    }

    #[tokio::test]
    async fn the_end_of_the_disc_does_not_restart_playback_and_tells_the_source() {
        // Regression (review 2026-07-27): `Play cdda://` set
        // `expecting_stream`, so the end of the disc (mpv idle) triggered the
        // restart machinery of network streams: `Activate` → `Play cdda://`
        // → the disc started over from track 1, indefinitely.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        // Single source: SourceCycle re-activates "cd", which answers `Play cdda://`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        // Eof of the disc: no restart, and the Source is told — it alone can
        // realign its view and its identity on "nothing plays anymore".
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Stop"));
    }

    #[tokio::test]
    async fn a_track_advance_by_the_player_is_relayed_to_the_source() {
        // mpv reports the advance, the core cannot fix an opaque identity:
        // it has the Source fix it, the only one that knows what "track 2"
        // means.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_event(Event::TrackChanged(2)).await;
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:PlayerTrack(2)"));
    }

    #[tokio::test]
    async fn a_track_advance_in_standby_is_not_relayed() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_event(Event::TrackChanged(2)).await;
        assert!(source_calls.lock().unwrap().is_empty(), "nothing must leave in standby");
    }
}
