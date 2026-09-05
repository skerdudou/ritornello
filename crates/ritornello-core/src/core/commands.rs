//! Remote and UI commands: playback/standby state machine, volume, tens, seeking within the track, held input, startup.

use super::*;

impl<P: Player> Core<P> {
    pub async fn handle_command(&mut self, cmd: Command) -> Result<()> {
        if self.standby && cmd != Command::Power {
            return Ok(());
        }
        let outcome = self.apply_command(cmd).await;
        // Publication on the way out of **every** command, rather than a call
        // in each one: volume, mute, standby and active source all change
        // here, and a forgotten branch would leave the UI showing a stale
        // state without anything flagging it. The channel deduplicates, so
        // publishing for nothing costs no frame. Published even on error: the
        // partial state reached is what the UI must show.
        self.publish_state();
        outcome
    }

    /// Absolute volume, the only way in for a setting that comes from no key:
    /// MPD's `setvol`. Same side effects as the relative step — mpv, disk,
    /// overlay — because a volume changed from the network must announce
    /// itself on screen like one changed from the remote.
    pub(super) async fn set_volume(&mut self, v: u8) -> Result<()> {
        self.volume = v.min(100);
        self.player.set_volume(self.volume).await?;
        self.persist();
        self.show_overlay().await;
        Ok(())
    }

    /// One volume step (±5), applied to mpv, persisted, shown as an overlay.
    /// Shared by fresh presses and held repeats; only the caller decides how
    /// to re-arm `volume_deadline`.
    pub(super) async fn step_volume(&mut self, up: bool) -> Result<()> {
        let v = self.volume as i16 + if up { 5 } else { -5 };
        self.set_volume(v.clamp(0, 100) as u8).await
    }

    /// Entry point for everything that used to call `handle_command`: fresh
    /// commands pass through unchanged; held (autorepeat) volume commands are
    /// paced by `volume_deadline`. Held on any other command is a no-op — the
    /// remote's autorepeat only means something for the volume.
    pub async fn handle_input(&mut self, msg: InputMessage) -> Result<()> {
        if !msg.held {
            return self.handle_command(msg.cmd).await;
        }
        if self.standby {
            return Ok(());
        }
        let up = match msg.cmd {
            Command::VolumeUp => true,
            Command::VolumeDown => false,
            _ => return Ok(()),
        };
        let Some(deadline) = self.volume_deadline else { return Ok(()) };
        if Instant::now() < deadline {
            return Ok(());
        }
        let outcome = self.step_volume(up).await;
        self.volume_deadline =
            Some(Instant::now() + Duration::from_millis(self.settings.volume_repeat_interval_ms.into()));
        // Same publication contract as `handle_command`: the UI must see the
        // new volume even if mpv errored mid-way.
        self.publish_state();
        outcome
    }

    /// New settings from `PUT /api/settings` (via the `select!` loop of main).
    /// No bounds check here: the HTTP layer validates, and tests rely on tiny
    /// timings.
    pub fn set_settings(&mut self, s: crate::state::Settings) {
        // Pushed into the cover cache, which is the only other holder of
        // these settings. Here and not in an arm of the `select!`:
        // `set_settings` is the **single** passage point of every settings
        // change — the HTTP route as well as loading at startup —, hence the
        // only place where propagation cannot be forgotten by a future
        // caller. Synchronous because `CoverCache` keeps these settings under
        // a `std::sync` lock precisely for this.
        self.covers.set_cover_settings(crate::cover::CoverSettings::from(&s));
        self.settings = s;
        self.persist();
    }

    /// Startup: what the process does with the active source when it
    /// launches, per `settings.startup_power`. Called once by `main`, which
    /// treats a failure as best-effort — startup must never put systemd in a
    /// restart loop.
    ///
    /// The `On` branch persists before waking: `start_in_standby` writes
    /// `standby: true` on the other side, and without this the file would
    /// keep saying "in standby" after a boot that woke everything up — a
    /// later switch to `Previous` would then resurrect a standby the device
    /// left behind long ago.
    pub async fn startup(&mut self) -> Result<()> {
        let in_standby = match self.settings.startup_power {
            StartupPower::On => false,
            StartupPower::Standby => true,
            StartupPower::Previous => self.persisted_standby,
        };
        if in_standby {
            return self.start_in_standby().await;
        }
        // `resume` is also the "wake" half of `Command::Power`, where the
        // flag is already lowered; here it is this method that lowers it,
        // so that the file describes an awake device.
        self.standby = false;
        self.persist();
        self.resume().await
    }

    /// Startup in standby (`settings.startup_power`): mpv is configured
    /// (volume, audio device) so a later wake starts right, but the active
    /// source is not woken and the display shows the standby status.
    ///
    /// `standby_status` is not resolved here: it already is, since
    /// construction (see its doc) — no sources_catalog read on this path.
    pub async fn start_in_standby(&mut self) -> Result<()> {
        self.standby = true;
        // Written before touching mpv, like the standby half of
        // `Command::Power`: what is on disk must describe the device even if
        // the calls below fail.
        self.persist();
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        self.publish_state();
        // A held key must re-press after standby: stale deadlines don't survive it.
        self.volume_deadline = None;
        Ok(())
    }

    pub(super) async fn apply_command(&mut self, cmd: Command) -> Result<()> {
        // Any command other than Plus10/Select abandons a pending tens
        // sequence: pressing volume mid-sequence is a change of mind, not a
        // step of it. When an offset was actually armed, its `+NN` overlay
        // must go with it: `player_state` gives the overlay absolute
        // priority, and none of the arms below (PlayPause, Stop, Next, Prev,
        // Eject) rewrite it on their own, so without clearing it here the
        // display would keep showing an offset that no longer applies until
        // the deadline expires on its own. `handle_command`'s trailing
        // `publish_state` picks up the clear — no need to publish here too.
        // The `!= 0` guard on `mem::take` keeps VolumeUp/Mute/Power
        // unaffected: they already overwrite or clear `self.overlay` right
        // after this.
        if !matches!(cmd, Command::Plus10 | Command::Select(_))
            && std::mem::take(&mut self.pending_tens) != 0
        {
            self.overlay = None;
        }
        match cmd {
            Command::Select(n) => {
                let tens = std::mem::take(&mut self.pending_tens);
                if tens != 0 {
                    // The consumed offset's overlay has said what it had to
                    // say; the source's own status takes over.
                    self.overlay = None;
                }
                // **Key 0 is worth ten, so each decade covers 1..10, 11..20,
                // 21..30 — never 1..9 then 10..19.** The owner asked for pages
                // of ten starting at 1, and the physical remote has to name the
                // very same groups as the web grid (see `PresetGrid.vue`),
                // so the change belongs here and not only in the page.
                //
                // What it buys beyond the pages: preset 10 used to need `+10`
                // then `0`, while `0` alone did nothing at all — a key that is
                // inert until you have pressed another one first. Now `0` picks
                // 10, `+10` `0` picks 20, and the last decade is no longer half
                // empty.
                //
                // Selections that are **not** a remote digit — the web grid, the
                // MPD server — carry an absolute preset number and reach this
                // arm with `pending_tens` at zero, so they pass through
                // untouched. Zero is not a preset number, so no caller can mean
                // "preset 0" here.
                let n = tens.saturating_add(if n == 0 { 10 } else { n });
                self.retry_count = 0;
                if let Some(action) = self.active_request(SourceReq::Select(n)).await? {
                    self.apply(action).await?;
                }
            }
            // `Next`/`Prev` now carry both semantics: the active source
            // decides (preset for the radio, track for the cd — see
            // `SourcePlugin::next`/`prev` of each plugin). Resetting
            // `retry_count` to 0 here is correct for a preset change (new
            // radio stream, a retry on the old one would make no sense) and
            // harmless for a cd track change (`retry_count` only concerns the
            // restart of an expected network stream, not cd playback):
            // nothing to distinguish between the two sources on this point.
            Command::Next => {
                self.retry_count = 0;
                if let Some(action) = self.active_request(SourceReq::Next).await? {
                    self.apply(action).await?;
                }
            }
            Command::Prev => {
                self.retry_count = 0;
                if let Some(action) = self.active_request(SourceReq::Prev).await? {
                    self.apply(action).await?;
                }
            }
            Command::Eject => {
                if let Some(action) = self.active_request(SourceReq::Eject).await? {
                    self.apply(action).await?;
                }
            }
            Command::VolumeUp | Command::VolumeDown => {
                self.step_volume(cmd == Command::VolumeUp).await?;
                self.volume_deadline = Some(
                    Instant::now() + Duration::from_millis(self.settings.volume_repeat_initial_ms.into()),
                );
            }
            Command::SetVolume(v) => {
                // No `volume_deadline` to re-arm: this is not a key, nothing
                // can be held.
                self.set_volume(v).await?;
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
                self.show_overlay().await;
            }
            Command::PlayPause => {
                if self.playback {
                    // Flip the belief **after** mpv has accepted, never
                    // before. The `?` propagates a `toggle_pause` failure and
                    // leaves `paused` intact: that is the very value
                    // `PlayerState.playback` publishes, and the one the MPD
                    // plugin compares its `pause 0`/`pause 1` against — a core
                    // that believes itself paused in front of an mpv that
                    // plays answers "paused" to a client whose music goes on,
                    // and the following `pause 0` is then judged a no-op and
                    // ignored.
                    self.player.toggle_pause().await?;
                    self.paused = !self.paused;
                } else {
                    // Nothing is loaded: `stop` **clears mpv's playlist**, so
                    // "toggle pause" has nothing left to resume. The Play key
                    // therefore did nothing at all after a Stop, on every
                    // source — measured on the radio as well as on files. We
                    // ask the active source to play again, which is exactly
                    // what the key promises.
                    //
                    // `playback` and not `expecting_stream`: the first says
                    // "something plays, of whatever nature", the second only
                    // holds for restartable streams. A pause touches neither —
                    // so resuming stays a simple toggle, without reloading.
                    if let Some(action) = self.active_request(SourceReq::Activate).await? {
                        self.apply(action).await?;
                    }
                }
            }
            Command::Stop => {
                self.expecting_stream = false;
                self.playback = false;
                self.player.stop().await?;
                // Forget the identity **before** notifying the Source: this
                // call clears the display's title, and an unreachable Source
                // would make us wait up to 5 s (timeout of
                // `SourceClient::request`) with the stopped track still on
                // screen.
                self.set_identity(None);
                // The Source was not consulted for this stop: tell it,
                // otherwise one that keeps its own playback state (the cd)
                // would keep it wrong and later announce metadata for a
                // stopped track. Best effort: a silent Source prevents
                // nothing.
                if let Err(e) = self.active_request(SourceReq::Stop).await {
                    tracing::debug!("stop notification to source: {e}");
                }
            }
            Command::Power => {
                self.standby = !self.standby;
                // Persist **before** notifying the Source, for the same
                // reason as at `SourceCycle` below: an unreachable Source
                // makes us wait up to 5 s, and `StartupPower::Previous` must
                // find the intended standby even if the power is cut during
                // that wait.
                self.persist();
                if self.standby {
                    let _ = self.active_request(SourceReq::Deactivate).await;
                    self.player.stop().await?;
                    self.expecting_stream = false;
                    self.playback = false;
                    // Same reason as above: the Source's answer to
                    // `Deactivate` is ignored, and the standby view that
                    // follows would bypass the guard of `handle_source_update`.
                    self.set_identity(None);
                    // The preset count and the status only make sense for the
                    // active Source: standby forgets both, and the next Source
                    // (activate/wake) will redeclare them if it has any.
                    // Without this clearing, the old source's status
                    // ("NO DISC") survived standby in memory, ready to
                    // reappear on wake before the Source had spoken again.
                    self.preset_count = None;
                    self.source_status = None;
                    // Same fate for the eject capability: in standby no
                    // command gets through anyway (`handle_command`), and the
                    // Source will redeclare it on wake.
                    self.can_eject = false;
                    // The volume/mute overlay does not survive entering
                    // standby: it keeps priority in `player_state`, and
                    // "VOLUME 65 %" stayed on screen for up to 2 s after
                    // power-off before the standby word appeared.
                    self.overlay = None;
                    // `standby_status` is not resolved here: it already is,
                    // since construction and every `set_locale` (see its
                    // doc) — never again at the moment standby is entered.
                    // A held key must re-press after standby: stale deadlines don't survive it.
                    self.volume_deadline = None;
                } else {
                    self.resume().await?;
                }
            }
            Command::SourceCycle => {
                // The active source may no longer be in the order: that is
                // the state `forget_dead_source` leaves — the plugin is gone,
                // the music goes on, and its name stays displayed. In that
                // case the Source key must start again from the **first**
                // available source. A `position().unwrap_or(0)` followed by
                // the `+ 1` skipped the first to go to the second, which made
                // one source unreachable from the keyboard until a full
                // round had been made.
                let next = match self.source_order.iter().position(|n| n == &self.active_source) {
                    Some(idx) => self.source_order.get((idx + 1) % self.source_order.len()).cloned(),
                    None => self.source_order.first().cloned(),
                };
                self.cycle_source(next).await?;
            }
            Command::SelectSource(name) => {
                // Unknown: silently ignored, like an unbound key. The MPD
                // plugin has already answered `ACK 50` on its side — it only
                // offers names received from the sources_catalog, so getting
                // here means the source disappeared in the meantime (a plugin
                // just switched off from the UI, for instance).
                if !self.source_order.iter().any(|n| n == &name) {
                    tracing::debug!("unknown source {name} ignored");
                    return Ok(());
                }
                // Already active: do nothing. A redundant `load` must not cut
                // what plays, and that is exactly what a client sends when
                // reopening its screen.
                if name != self.active_source {
                    // `Some(name)`: `cycle_source` accepts `None` — "no
                    // source at all" — but this path always designates a
                    // name, the guard above having checked it in the order.
                    self.cycle_source(Some(name)).await?;
                }
            }
            Command::Plus10 => {
                let next = self.pending_tens.saturating_add(10);
                self.pending_tens = match self.preset_count {
                    // Wrap past the last useful decade. A decade now covers
                    // `offset + 1 ..= offset + 10` (key 0 is worth ten, see
                    // `Select`), so the last one that holds anything starts at
                    // `((count - 1) / 10) * 10`: for 20 stations that is 10,
                    // whose decade is 11..20 — offset 20 would name 21..30 and
                    // hold nothing. Under the previous reading, where a decade
                    // was 10..19, offset 20 *was* needed to reach station 20;
                    // it no longer is, and allowing it would cost one dead
                    // press per cycle.
                    //
                    // `saturating_sub` guards the empty source: with no preset
                    // at all the only useful offset is zero.
                    Some(count) if next > (count.saturating_sub(1) / 10) * 10 => 0,
                    // No known count: saturate, don't wrap — we can't know
                    // where the end is.
                    None => next.min(240),
                    _ => next,
                };
                if self.pending_tens == 0 {
                    self.overlay = None;
                } else {
                    self.show_tens_overlay().await;
                }
            }
            Command::SeekForward | Command::SeekBackward => {
                // Silently ignored on non-seekable content: the key behaves
                // like an unbound key, which the remote already knows how to
                // do. A message would teach nothing to whoever just pressed.
                if self.playback && !self.expecting_stream {
                    let step = i64::from(self.settings.seek_step_s);
                    let delta = if cmd == Command::SeekForward { step } else { -step };
                    self.player.seek_relative(delta).await?;
                    self.refresh_position().await;
                }
            }
            Command::SeekTo(position_s) => {
                if self.playback && !self.expecting_stream {
                    self.player.seek_absolute(position_s).await?;
                    self.refresh_position().await;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    async fn standby_blocks_everything_but_power() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        core.handle_command(Command::Select(3)).await.unwrap();
        // no new "play" call after standby until Power is pressed again
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 1);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 2);
    }

    #[tokio::test]
    async fn commands_without_any_source_do_nothing_and_do_not_panic() {
        // The thirteen requests to the active source went through the same
        // `panic!`: the slightest remote key on a device without a source
        // stopped the core. Without a source, a command **does nothing**, and
        // the log says so at `debug` — this is not an anomaly.
        let (mut core, _rx, dir) = setup_without_source();
        for cmd in [
            Command::Select(1),
            Command::Next,
            Command::Prev,
            Command::Eject,
            Command::Stop,
            Command::PlayPause,
            Command::SourceCycle,
            // Standby, then wake: the second goes through `resume` again.
            Command::Power,
            Command::Power,
        ] {
            let label = format!("{cmd:?}");
            core.handle_command(cmd).await.unwrap_or_else(|e| panic!("{label}: {e}"));
        }
        // The two player events that notify the source, and the stream
        // restart: same calls, same cleared table.
        core.handle_event(Event::TrackChanged(2)).await;
        core.handle_event(Event::PlaybackIdle).await;
        core.retry_stream().await.unwrap();
        assert_eq!(core.active_source(), "", "no command could have designated a source");
        drop(dir);
    }

    #[tokio::test]
    async fn the_play_key_restarts_when_nothing_plays() {
        // Reported defect, and it affected **all** sources: `stop` clears
        // mpv's playlist, so "toggle pause" had nothing left to resume and
        // the Play key did nothing at all. Measured on the radio as well as
        // on files before the fix.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://fip")).await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        player_calls.lock().unwrap().clear();

        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["play http://fip".to_string()],
            "the active source must be asked again, not a pause into the void"
        );
    }

    #[tokio::test]
    async fn the_play_key_toggles_pause_when_playing() {
        // Guard of the previous test: a pause must stay a pause, and not
        // become a reload that would start again from the beginning of the
        // track.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://fip")).await.unwrap();
        player_calls.lock().unwrap().clear();

        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["pause".to_string()]);
        // And a second time: pausing does not "stop playing", so resuming
        // stays a simple toggle.
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["pause".to_string(), "pause".to_string()]);
    }

    #[tokio::test]
    async fn pause_and_resume_are_readable_in_the_published_state() {
        // The most read field of MPD's `status` command: without it, no
        // client can show the right button.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // starts playback
        assert_eq!(core.player_state().playback, Playback::Playing);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Paused);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Playing);
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Stopped);
    }

    #[tokio::test]
    async fn an_mpv_failure_does_not_change_the_core_belief_about_pause() {
        // `paused` was flipped **before** `toggle_pause`, so an mpv failure
        // left the core believing the opposite of the truth. This is not
        // cosmetic: it is the value `PlayerState.playback` publishes, and the
        // one the MPD plugin compares its `pause 0`/`pause 1` against. A core
        // that believes itself paused in front of an mpv that plays answers
        // "paused" to a client whose music goes on, then judges the following
        // `pause 0` a no-op.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // starts playback
        assert_eq!(core.player_state().playback, Playback::Playing);

        core.player.pause_fails.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            core.handle_command(Command::PlayPause).await.is_err(),
            "the mpv failure must propagate, it must not be swallowed"
        );
        assert_eq!(
            core.player_state().playback,
            Playback::Playing,
            "mpv refused: the core must keep saying what is true"
        );

        // And resuming the dialogue puts the toggle back to work: the flag
        // was not damaged, it simply did not move.
        core.player.pause_fails.store(false, std::sync::atomic::Ordering::SeqCst);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Paused);
    }

    #[tokio::test]
    async fn a_pause_does_not_survive_a_new_play() {
        // The only clearing of `paused` is the one of the applied `Play`: if
        // it were forgotten, yesterday's pause would make a fresh playback
        // "paused".
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // starts playback (radio, http://fip)
        core.handle_command(Command::PlayPause).await.unwrap(); // pauses
        assert_eq!(core.player_state().playback, Playback::Paused);
        // Selects another radio preset (`http://inter`): a new `Play` is
        // applied, and `paused` must fall back through this path alone.
        core.handle_command(Command::Select(3)).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Playing);
    }

    #[tokio::test]
    async fn standby_says_stopped_even_if_pause_was_set() {
        // This test isolates only the forgetting of the `paused` flag:
        // `Command::Power` sets `standby = true` and `playback = false` in the
        // same step, so it cannot tell which of the two conditions does the
        // work. What it proves: a `paused` set earlier must not leak into the
        // state reported during standby.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // starts playback
        core.handle_command(Command::PlayPause).await.unwrap(); // pauses
        assert_eq!(core.player_state().playback, Playback::Paused);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Stopped);
    }

    #[tokio::test]
    async fn absolute_volume_replaces_the_volume_and_bounds_it() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetVolume(40)).await.unwrap();
        assert_eq!(core.player_state().volume, 40);
        core.handle_command(Command::SetVolume(200)).await.unwrap();
        assert_eq!(core.player_state().volume, 100, "upper bound");
        core.handle_command(Command::SetVolume(0)).await.unwrap();
        assert_eq!(core.player_state().volume, 0);
    }

    #[tokio::test]
    async fn absolute_volume_writes_an_overlay_like_the_relative_step() {
        // A volume changed from the network must announce itself on screen
        // like one changed from the remote.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetVolume(40)).await.unwrap();
        assert!(core.player_state().overlay.is_some());
    }

    #[tokio::test]
    async fn held_volume_is_ignored_before_the_initial_delay() {
        // **Driven** deadline, never waited on. The previous version relied
        // on two consecutive lines executing in less than the 30 ms of the
        // initial timeout: under load -- a `cargo test --workspace` still
        // compiling while it tests -- scheduling exceeded that margin and the
        // test fell, once every few dozen runs. What it verifies no longer
        // depends on the machine's speed.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(quick_settings());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 60 -> 65, arms the deadline
        // Pushed far away: the repeat has no reason to happen, however slow
        // what precedes may be.
        core.volume_deadline = Some(Instant::now() + Duration::from_secs(60));
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 65, "a repeat before the initial timeout does nothing");
    }

    #[tokio::test]
    async fn held_volume_repeats_after_the_delay_then_at_the_interval() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(quick_settings());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65

        // Deadline reached: the first repeat goes through. `Instant::now()`
        // is already in the past when `handle_input` reads it again -- time
        // does not go backwards, so this trigger is certain.
        let set = Instant::now();
        core.volume_deadline = Some(set);
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 70, "first repeat after the initial timeout");

        // It re-armed the deadline for the next interval. Compared to the one
        // we **set**, and not to `Instant::now()`: the new one is "instant of
        // the repeat + interval", so it is later than `set` whatever happens.
        // Comparing it to the present would reintroduce the race this test
        // exists to remove.
        let rearmed = core.volume_deadline.expect("the interval must be re-armed");
        assert!(rearmed > set, "the deadline was not re-armed after the repeat");

        // A deadline in the future blocks the next repeat.
        core.volume_deadline = Some(Instant::now() + Duration::from_secs(60));
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 70);

        // Interval elapsed: one more repeat, and only one.
        core.volume_deadline = Some(Instant::now());
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 75, "then one per interval");
    }

    #[tokio::test]
    async fn held_volume_without_an_initial_press_does_nothing() {
        // A held event with no prior press (core restarted mid-hold): no
        // deadline is armed, nothing moves.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(quick_settings());
        core.resume().await.unwrap();
        core.handle_input(InputMessage { cmd: Command::VolumeDown, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 60);
    }

    #[tokio::test]
    async fn held_on_a_non_volume_command_is_ignored() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_input(InputMessage { cmd: Command::Next, held: true }).await.unwrap();
        assert!(source_calls.lock().unwrap().is_empty(), "a held Next must not reach the source");
    }

    #[tokio::test]
    async fn held_volume_is_blocked_in_standby() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(quick_settings());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby
        // No sleep: standby short-circuits `handle_input` **before** looking
        // at the deadline, so waiting for it to expire proved nothing. The
        // deadline is set in the past so the test fails if that
        // short-circuit disappeared.
        core.volume_deadline = Some(Instant::now());
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 65);
    }

    #[tokio::test]
    async fn non_held_handle_input_is_equivalent_to_handle_command() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_input(InputMessage::from(Command::Select(3))).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn plus10_is_shown_and_pushes_its_deadline_back() {
        // Each press shows the total (+10, +20) in the overlay, with the same
        // deadline as the volume.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(core.overlay_deadline().is_some());
        match state_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Tens { offset, text, .. }) => {
                assert_eq!(offset, 10);
                assert_eq!(text, "PRESET +10");
            }
            other => panic!("expected a Tens overlay, got {other:?}"),
        };
        core.handle_command(Command::Plus10).await.unwrap();
        match state_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Tens { offset, text, .. }) => {
                assert_eq!(offset, 20);
                assert_eq!(text, "PRESET +20");
            }
            other => panic!("expected a Tens overlay, got {other:?}"),
        };
    }

    #[tokio::test]
    async fn the_offset_is_consumed_by_the_digit_key() {
        // +10 then 4 = preset 14; the offset does not survive its
        // consumption.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_with_count(Some(23)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Select(4)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(14)")));
        core.handle_command(Command::Select(4)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(4)")));
    }

    #[tokio::test]
    async fn the_zero_key_alone_is_worth_ten() {
        // **The change asked for by the owner**: decades cover 1..10, 11..20,
        // 21..30. The 0 key therefore names the tenth of its decade, and
        // alone it is worth 10. Previously it did *nothing* — an inert key
        // until `+10` had been pressed first.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_with_count(Some(23)));
        core.handle_command(Command::Select(0)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(10)")));
    }

    #[tokio::test]
    async fn zero_reaches_the_top_of_its_decade() {
        // 20 stations: `+10` then 0 = 20, the current decade being 11..20.
        // One more `+10` would wrap — see the next test.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_with_count(Some(20)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Select(0)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(20)")));
    }

    #[tokio::test]
    async fn plus10_wraps_after_the_last_decade() {
        // 23 stations: decades 1..10, 11..20, 21..23 — useful offsets 10 and
        // 20. The third press goes back to zero and turns the overlay off,
        // like the web window.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_with_count(Some(23)));
        for _ in 0..3 {
            core.handle_command(Command::Plus10).await.unwrap();
        }
        assert!(core.overlay_deadline().is_none());
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn the_last_useful_decade_does_not_exceed_the_count() {
        // The counterpart of the bound: for a count exactly on a decade (20),
        // offset 20 would name 21..30, where there is nothing. It must wrap,
        // and that is what the old bound `(count / 10) * 10` let through —
        // it needed that offset when a decade was worth 10..19.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", update_with_count(Some(20)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(
            core.overlay_deadline().is_none(),
            "the second +10 must wrap to zero on 20 stations"
        );
    }

    #[tokio::test]
    async fn another_command_abandons_the_offset() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn abandoning_the_offset_also_clears_its_overlay() {
        // `VolumeUp` hides the defect (it writes its own overlay right
        // after): `PlayPause` writes no overlay, so nothing must clear the
        // `+NN` in its place other than the abandon guard itself. Without
        // the fix, the overlay stayed on screen until its deadline while the
        // offset was already abandoned.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(core.overlay_deadline().is_some(), "the +10 overlay must be displayed");
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(
            core.overlay_deadline().is_none(),
            "the +NN overlay must disappear with the abandoned offset"
        );
    }

    #[tokio::test]
    async fn the_overlay_deadline_forgets_the_offset() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        core.expire_overlay();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn without_a_known_count_the_offset_saturates_without_wrapping() {
        // No declared count: we do not know where the end is, so no wrap —
        // saturation at 240.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        for _ in 0..30 {
            core.handle_command(Command::Plus10).await.unwrap();
        }
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(243)")));
    }

    #[tokio::test]
    async fn set_settings_persists() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        core.set_settings(crate::state::Settings {
            volume_repeat_initial_ms: 800,
            volume_repeat_interval_ms: 250,
            startup_power: StartupPower::Previous,
            ..Default::default()
        });
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert_eq!(st.settings.startup_power, StartupPower::Previous);
    }

    #[tokio::test]
    async fn the_seek_keys_act_on_finite_content() {
        let (mut core, calls, _, _, _dir) = setup();
        // Finite content: switch from `radio` (default active source) to `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekBackward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        let log = calls.lock().unwrap().clone();
        assert!(log.contains(&"seek_relative 10".to_string()), "{log:?}");
        assert!(log.contains(&"seek_relative -10".to_string()), "{log:?}");
        assert!(log.contains(&"seek_absolute 198".to_string()), "{log:?}");
    }

    /// On a live stream, the key does nothing — like an unbound key. No
    /// message, no frame: the content is not seekable, and saying so would
    /// teach nothing to whoever just pressed.
    #[tokio::test]
    async fn the_seek_keys_are_ignored_on_a_stream() {
        let (mut core, calls, _, _, _dir) = setup();
        // Stream: `radio` is already the active source, `PlayPause` makes it play.
        core.handle_command(Command::PlayPause).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        assert!(
            calls.lock().unwrap().iter().all(|c| !c.starts_with("seek_")),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn the_seek_step_follows_the_setting() {
        let (mut core, calls, _, _, _dir) = setup();
        // `set_settings` already exists (it serves the `PUT /api/settings` route).
        core.set_settings(crate::state::Settings { seek_step_s: 30, ..Default::default() });
        // Finite content: switch from `radio` to `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"seek_relative 30".to_string()));
    }

    #[tokio::test]
    async fn an_ephemeral_message_disarms_a_pending_offset() {
        // A source's ephemeral message ("empty preset") borrows the same
        // overlay slot as the +NN total and steals it: without disarming the
        // offset here, the next digit press would still compose the old
        // offset while the screen no longer shows +NN but the source's
        // message.
        let (mut core, _pc, source_calls, mut state_rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(matches!(state_rx.borrow_and_update().overlay, Some(Overlay::Tens { .. })));

        let mut ephemeral = bare_update();
        ephemeral.transient = true;
        ephemeral.status = Some("empty preset".into());
        core.handle_source_update("radio", ephemeral);

        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(
            source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")),
            "with no armed offset, Select(3) must ask for preset 3"
        );
        assert!(
            !source_calls.lock().unwrap().iter().any(|c| c.contains("Select(13)")),
            "the offset abandoned by the ephemeral message must not be applied"
        );
    }

    #[tokio::test]
    async fn startup_in_standby_applies_the_volume_without_waking_the_source() {
        let (mut core, player_calls, source_calls, mut state_rx, _d) = setup();
        core.start_in_standby().await.unwrap();
        // mpv is configured (volume applied) so waking later starts right...
        // (FakePlayer::set_volume records "vol {v}", see FakePlayer above.)
        assert!(player_calls.lock().unwrap().iter().any(|c| c.starts_with("vol ")));
        // ...but the source was NOT woken, and the display shows standby.
        assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")), "no Wake in standby");
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));
        assert!(core.player_state().standby);
        // Power then wakes normally.
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.player_state().standby);
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")));
    }

    /// The three values of `startup_power`, on the only observable criterion:
    /// is the source woken? `Previous` is tested in both directions,
    /// otherwise a `Previous` treated as `On` would pass half the test.
    #[tokio::test]
    async fn startup_follows_the_power_on_setting() {
        async fn wakes(startup_power: StartupPower, persisted_standby: bool) -> bool {
            let persisted = PersistedState {
                standby: persisted_standby,
                settings: crate::state::Settings { startup_power, ..Default::default() },
                ..Default::default()
            };
            let (mut core, _pc, source_calls, _rx, _d) = setup_persisted(persisted);
            core.startup().await.unwrap();
            // The guard is a temporary of the tail expression, so edition
            // 2024 drops it *before* the block's locals and it cannot
            // outlive `source_calls`. Up to edition 2021 the reverse held,
            // and this needed a binding of its own to release the lock.
            source_calls.lock().unwrap().iter().any(|c| c.contains("Wake"))
        }

        assert!(wakes(StartupPower::On, true).await, "\"on\" ignores the standby on disk");
        assert!(!wakes(StartupPower::Standby, false).await, "\"standby\" never wakes");
        assert!(wakes(StartupPower::Previous, false).await, "was on: we relaunch");
        assert!(!wakes(StartupPower::Previous, true).await, "was in standby: we stay there");
    }

    #[tokio::test]
    async fn stop_is_notified_to_the_active_source() {
        // `Command::Stop` is the only command that changes the playback state
        // without consulting the Source: without this notification, a Source
        // that keeps its own playback state (the cd) would keep it wrong.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Stop"));
    }
}
