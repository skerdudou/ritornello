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
        // The play mode, like the language just above: every wired source is
        // owed it at boot and at every wake, not only the active one — a
        // source made active later by a switch would otherwise start from
        // whatever default `set_play_mode` never corrected.
        self.push_play_mode().await;
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
            Event::Title(_) => {
                self.retry_count = 0;
                return EventOutcome::StreamAlive;
            }
            // mpv's own confirmation that something is really playing — the
            // one signal `played_since_play` waits for. Separate from
            // `Title` above (which shares its verdict on `retry_count`
            // only): an ICY title never proves playback on its own (a
            // station can send one then go silent), so it must not stand in
            // for this confirmation.
            Event::PlaybackActive => {
                self.retry_count = 0;
                self.played_since_play = true;
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
            //
            // The reply can carry an action of its own — the cd answers a
            // track notification with `PlayerChapter(n)` when it owed a seek
            // that could not happen before mpv confirmed the disc was open
            // (see `pending_chapter` in the cd plugin). Before this arm
            // applied it, that action was silently dropped: the identity on
            // screen moved to the resumed track, but mpv itself stayed on
            // whatever it had opened, so the sound did not follow.
            Event::TrackChanged(n) => {
                if !self.standby {
                    match self.active_request(SourceReq::PlayerTrack(n)).await {
                        Ok(Some(action)) => {
                            if let Err(e) = self.apply(action).await {
                                tracing::debug!("applying the source's track-notification action: {e}");
                            }
                        }
                        Ok(None) => {}
                        Err(e) => tracing::debug!("track notification to source: {e}"),
                    }
                }
            }
            Event::PlaybackIdle => {
                if !self.standby && self.expecting_stream {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    return EventOutcome::RetryIn(delay);
                }
                // **The discriminant.** mpv goes idle after every stop,
                // commanded ones included: every commanded path (`Command::
                // Stop`, `Command::Power` entering standby, `cycle_source`)
                // finishes setting `playback = false` synchronously, as part
                // of handling that command, strictly before the core ever
                // gets to process the idle notification mpv sends back. So
                // an idle arriving while the core still believed it was
                // playing is the only one that can mean "the content ran
                // out" rather than "the user (or a source switch) stopped
                // it". `played_since_play` rules out the third case, a list
                // that was asked to play but never actually opened (see its
                // doc): that one is neither a user's stop nor a real ending.
                let ending = self.playback && self.played_since_play;
                self.playback = false;
                if self.standby {
                    return EventOutcome::Nothing;
                }
                // Eof of **normal** playback (end of disc, end of a file
                // list, or a real stop): tell the Source, the only one able
                // to realign its playback state, its view and its identity
                // — the core cannot invent "nothing plays anymore" in its
                // place, the identity is opaque. Without this, the end of a
                // disc left the last track and its metadata displayed
                // indefinitely.
                //
                // `EndOfContent` only on a real ending: it is what lets a
                // Source open another pass under random/repeat-all, which
                // must never happen on a Stop the user asked for.
                // Idempotent otherwise when the stop comes from a command
                // (the Source has already been told by `Command::Stop`).
                let req = if ending { SourceReq::EndOfContent } else { SourceReq::Stop };
                match self.active_request(req).await {
                    // The answer is **applied**, not logged and dropped —
                    // the same class of defect just fixed above for
                    // `TrackChanged`. Without this, "the Source says
                    // whether there is another pass" could not work at all.
                    Ok(Some(action)) => {
                        if ending {
                            // Second safety net (see `last_pass_reopen`'s own
                            // doc): a floor between two pass reopenings,
                            // independent of whatever judged this one
                            // genuine. Costs nothing on the honest path — a
                            // real pass plays for far longer than
                            // `RETRY_BASE` — and bounds a tight, dishonest
                            // loop to the same pace already imposed on
                            // streams.
                            if let Some(last) = self.last_pass_reopen {
                                let elapsed = last.elapsed();
                                if elapsed < RETRY_BASE {
                                    tokio::time::sleep(RETRY_BASE - elapsed).await;
                                }
                            }
                            self.last_pass_reopen = Some(tokio::time::Instant::now());
                        }
                        if let Err(e) = self.apply(action).await {
                            tracing::warn!("applying the answer to an ending: {e}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::debug!("stop notification to source: {e}"),
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
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: Arc::new(Mutex::new(Vec::new())), ..Default::default() }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let manifest_order = declared_order(&sources);
        let mut core = Core::new(player, Wiring { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, manifest_order, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
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
    async fn a_list_is_loaded_already_positioned() {
        // **One operation, not two**, and this is the defect this test now
        // forbids. Loading first and correcting the position afterwards left
        // mpv the time to genuinely open the list's first entry: measured on
        // mpv 0.37, the `path` property is published for entry 0 before the
        // reposition takes effect. The core then took that entry for what was
        // playing — it read a cover off it, on a network share, and made the
        // display flip through a track nobody had asked for.
        //
        // Carrying the index into the load is what makes that window
        // impossible, and the player interface no longer offers any way to
        // express the old sequence.
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
            vec!["load_list /var/lib/ritornello/plugin-files.m3u start=4".to_string()]
        );
    }

    #[tokio::test]
    async fn a_list_without_a_declared_index_says_so_explicitly() {
        // The counterpart, and it is not cosmetic: mpv's starting index is a
        // **persistent** option — measured, a second `loadlist` sent without
        // touching it starts again at the index the previous load declared.
        // "Nothing declared" must therefore travel as an explicit value all
        // the way to the player, otherwise a list loaded after a resume would
        // silently start on the resumed track.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("/var/lib/ritornello/plugin-files.m3u").playlist().finite())
            .await
            .unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["load_list /var/lib/ritornello/plugin-files.m3u start=auto".to_string()]
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
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone(), ..Default::default() }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let manifest_order = declared_order(&sources);
        let mut core = Core::new(player, Wiring { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, manifest_order, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
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
    async fn a_chapter_action_moves_the_player_without_reloading() {
        // Tracks of an audio CD are chapters of a single mpv entry: a chapter
        // action must reach the player as a seek, never as a fresh load —
        // reloading would reopen the disc and cut the continuously-mixed
        // join between tracks.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::PlayerChapter(4)).await.unwrap();
        let calls = player_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "chapter 4"), "{calls:?}");
        assert!(
            !calls.iter().any(|c| c.starts_with("load_list") || c.starts_with("play ")),
            "a chapter is a seek, never a reload: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_track_notification_action_is_applied_not_dropped() {
        // Regression (review 1, C1): `active_request` returns the action the
        // Source's reply carries, but this arm used to look only at the
        // `Err` case. The cd's own resume-then-seek mechanism answers a
        // `PlayerTrack` notification with `PlayerChapter(n)` — a real
        // command for the player, not a spontaneous refresh — and it was
        // silently dropped: the identity on screen moved to the resumed
        // track while mpv itself stayed wherever it had opened.
        let (mut core, player_calls, _sc, _rx, _d) = setup_persisted(PersistedState {
            active_source: "cd".into(),
            ..PersistedState::default()
        });
        core.handle_event(Event::TrackChanged(0)).await;
        let calls = player_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "chapter 4"), "{calls:?}");
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

    #[tokio::test]
    async fn an_ending_is_told_apart_from_a_commanded_stop() {
        // Pressing Stop makes mpv go idle too. Emitting the end-of-content
        // signal on that idle would restart playback under "repeat all" —
        // the very defect this signal exists to remove, moved one floor up.
        // `Command::Stop` sets `playback = false` before it ever talks to
        // mpv, so the idle that follows finds `playback` already false: the
        // discriminant below reads that as "not an ending".
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Stop).await.unwrap();
        core.handle_event(Event::PlaybackIdle).await;
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:Stop"), "{calls:?}");
        assert!(!calls.iter().any(|c| c == "radio:EndOfContent"), "a user's stop is not an ending: {calls:?}");
    }

    #[tokio::test]
    async fn a_real_ending_reaches_the_source_as_such() {
        // The counterpart: nobody told the core to stop, so `playback` is
        // still true when the idle arrives — `played_since_play` (set by the
        // `PlaybackActive` just below) is what tells this idle apart from
        // one firing on content that never actually opened (see the next
        // test).
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.apply(SourceAction::play("/tmp/list.m3u").playlist().finite()).await.unwrap();
        core.handle_event(Event::PlaybackActive).await;
        core.handle_event(Event::PlaybackIdle).await;
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:EndOfContent"));
    }

    #[tokio::test]
    async fn the_answer_to_an_ending_is_executed_not_dropped() {
        // Regression guard for the same class of defect just fixed above for
        // `TrackChanged` (see `a_track_notification_action_is_applied_not_dropped`):
        // without applying the reply, "the source says whether there is
        // another pass" cannot work at all — the core used to only log the
        // `Err` case here and throw away any action carried by an `Ok`.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("/tmp/list.m3u").playlist().finite()).await.unwrap();
        core.handle_event(Event::PlaybackActive).await;
        core.handle_event(Event::PlaybackIdle).await;
        let calls = player_calls.lock().unwrap();
        assert!(
            calls.iter().filter(|c| c.starts_with("load_list")).count() >= 2,
            "the new pass answered by the source must actually load: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_second_pass_reopened_too_soon_is_held_back_by_the_floor() {
        // Regression #5 (whole-branch review): `played_since_play`'s own
        // confirmation is not measured to be reliable, and may (per mpv's
        // own doc, in the wrong direction) turn true before the demuxer
        // has actually opened anything — a sleeping network share could
        // then keep looking like a genuine ending and reopen a fresh pass
        // at full speed, forever. `last_pass_reopen` is a second,
        // unconditional floor against exactly that, independent of
        // whatever judged any one ending genuine.
        //
        // Simulated clock: the assertions are on how much *virtual* time
        // elapsed, and `tokio::time::pause` lets the runtime fast-forward
        // through the floor's own `sleep` instead of the test actually
        // waiting on it.
        tokio::time::pause();
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("/tmp/list.m3u").playlist().finite()).await.unwrap();
        core.handle_event(Event::PlaybackActive).await;

        let before_first = tokio::time::Instant::now();
        core.handle_event(Event::PlaybackIdle).await; // first ending: reopens at once
        assert!(
            tokio::time::Instant::now() - before_first < RETRY_BASE,
            "nothing has reopened before this: no floor to wait out yet"
        );

        // The fixture answers every `EndOfContent` with a fresh pass (see
        // `setup`), so a second ending right away is exactly the runaway
        // loop this floor exists for.
        core.handle_event(Event::PlaybackActive).await;
        let before_second = tokio::time::Instant::now();
        core.handle_event(Event::PlaybackIdle).await;
        assert!(
            tokio::time::Instant::now() - before_second >= RETRY_BASE,
            "a second reopening this soon must be held back by the floor"
        );
    }

    #[tokio::test]
    async fn a_content_that_never_played_does_not_start_another_pass() {
        // A sleeping NAS: loadlist, immediate idle, ending, loadlist… at full
        // speed, outside the exponential backoff which only covers streams.
        // No `PlaybackActive` fires here, so `played_since_play` stays false
        // and the idle is read as "never opened", not as "ran out".
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.apply(SourceAction::play("/tmp/gone.m3u").playlist().finite()).await.unwrap();
        core.handle_event(Event::PlaybackIdle).await; // no PlaybackActive in between
        assert!(!source_calls.lock().unwrap().iter().any(|c| c == "radio:EndOfContent"));
    }
}
